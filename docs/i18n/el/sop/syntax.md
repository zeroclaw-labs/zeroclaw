# Αναφορά Σύνταξης SOP

Οι ορισμοί SOP φορτώνονται από υποκαταλόγους κάτω από το `sops_dir` (προεπιλογή: `<workspace>/sops`).

## 1. Διάταξη Καταλόγου

```text
<workspace>/sops/
  deploy-prod/
    SOP.toml
    SOP.md
```

Κάθε SOP πρέπει να έχει `SOP.toml`. Το `SOP.md` είναι προαιρετικό, αλλά εκτελέσεις χωρίς αναλυμένα βήματα θα αποτύχουν κατά την επικύρωση.

## 2. `SOP.toml`

```toml
[sop]
name = "deploy-prod"
description = "Deploy service to production"
version = "1.0.0"
priority = "high"              # low | normal | high | critical
execution_mode = "supervised"  # auto | supervised | step_by_step | priority_based
cooldown_secs = 300
max_concurrent = 1

[[triggers]]
type = "webhook"
path = "/sop/deploy"

[[triggers]]
type = "manual"

[[triggers]]
type = "mqtt"
topic = "ops/deploy"
condition = "$.env == \"prod\""
```

## 3. Μορφή Βημάτων `SOP.md`

Τα βήματα αναλύονται από την ενότητα `## Steps`.

```md
## Steps

1. **Preflight** — Check service health and release window.
   - tools: http_request

2. **Deploy** — Run deployment command.
   - tools: shell
   - requires_confirmation: true
```

Συμπεριφορά parser:

- Αριθμημένα στοιχεία (`1.`, `2.`, ...) ορίζουν τη σειρά βημάτων.
- Κεφαλαίο έντονο κείμενο (`**Τίτλος**`) γίνεται τίτλος βήματος.
- `- tools:` αντιστοιχίζεται σε `suggested_tools`.
- `- requires_confirmation: true` επιβάλλει έγκριση για αυτό το βήμα.

## 4. Τύποι Trigger

| Τύπος | Πεδία | Σημειώσεις |
|---|---|---|
| `manual` | κανένα | Ενεργοποιείται από το εργαλείο `sop_execute` (όχι από CLI `zeroclaw sop run`). |
| `webhook` | `path` | Ακριβής αντιστοίχιση με μονοπάτι αιτήματος (`/sop/...` ή `/webhook`). |
| `mqtt` | `topic`, προαιρετικό `condition` | Το MQTT topic υποστηρίζει wildcards `+` και `#`. |
| `cron` | `expression` | Υποστηρίζει 5, 6 ή 7 πεδία (τα 5-πεδία λαμβάνουν δευτερόλεπτα εσωτερικά). |
| `peripheral` | `board`, `signal`, προαιρετικό `condition` | Αντιστοιχεί `"{board}/{signal}"`. |

## 5. Σύνταξη Condition

Το `condition` αξιολογείται fail-closed (μη έγκυρη συνθήκη/payload => καμία αντιστοίχιση).

- Συγκρίσεις JSON path: `$.value > 85`, `$.status == "critical"`
- Άμεσες αριθμητικές συγκρίσεις: `> 0` (χρήσιμο για απλά payloads)
- Τελεστές: `>=`, `<=`, `!=`, `>`, `<`, `==`

## 6. Επικύρωση

Χρησιμοποιήστε:

```bash
zeroclaw sop validate
zeroclaw sop validate <name>
```

Η επικύρωση προειδοποιεί για κενά ονόματα/περιγραφές, απόντα triggers, απόντα βήματα και κενά στην αρίθμηση βημάτων.
