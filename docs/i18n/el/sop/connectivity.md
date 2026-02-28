# Συνδεσιμότητα SOP & Event Fan-In

Αυτό το έγγραφο περιγράφει πώς τα εξωτερικά συμβάντα ενεργοποιούν εκτελέσεις SOP.

## Γρήγορα Μονοπάτια

- [Ενσωμάτωση MQTT](#2-ενσωμάτωση-mqtt)
- [Ενσωμάτωση Webhook](#3-ενσωμάτωση-webhook)
- [Ενσωμάτωση Cron](#4-ενσωμάτωση-cron)
- [Προεπιλογές Ασφαλείας](#5-προεπιλογές-ασφαλείας)
- [Αντιμετώπιση Προβλημάτων](#6-αντιμετώπιση-προβλημάτων)

## 1. Επισκόπηση

Το ZeroClaw δρομολογεί συμβάντα MQTT/webhook/cron/περιφερειακών μέσω ενός ενοποιημένου SOP dispatcher (`dispatch_sop_event`).

Βασικές συμπεριφορές:

- **Συνεπής αντιστοίχιση trigger:** ένα μονοπάτι matcher για όλες τις πηγές συμβάντων.
- **Έλεγχος εκκίνησης εκτέλεσης:** οι εκτελέσεις που ξεκινούν αποθηκεύονται μέσω `SopAuditLogger`.
- **Ασφάλεια headless:** σε πλαίσια εκτός agent-loop, οι ενέργειες `ExecuteStep` καταγράφονται ως εκκρεμείς (χωρίς σιωπηλή εκτέλεση).

## 2. Ενσωμάτωση MQTT

### 2.1 Ρύθμιση

Ρυθμίστε την πρόσβαση στον broker στο `config.toml`:

```toml
[channels_config.mqtt]
broker_url = "mqtts://broker.example.com:8883"  # χρησιμοποιήστε mqtt:// για plaintext
client_id = "zeroclaw-agent-1"
topics = ["sensors/alert", "ops/deploy/#"]
qos = 1
username = "mqtt-user"      # προαιρετικό
password = "mqtt-password"  # προαιρετικό
use_tls = true              # πρέπει να ταιριάζει με το scheme (mqtts:// => true)
```

### 2.2 Ορισμός Trigger

Στο `SOP.toml`:

```toml
[[triggers]]
type = "mqtt"
topic = "sensors/alert"
condition = "$.severity >= 2"
```

Το payload MQTT προωθείται στο payload συμβάντος SOP (`event.payload`) και εμφανίζεται στο πλαίσιο βήματος.

## 3. Ενσωμάτωση Webhook

### 3.1 Endpoints

- **`POST /sop/{*rest}`**: Endpoint αποκλειστικά για SOP. Επιστρέφει `404` αν δεν υπάρχει αντιστοίχιση. Χωρίς fallback LLM.
- **`POST /webhook`**: endpoint chat. Επιχειρεί πρώτα αποστολή SOP· αν δεν υπάρχει αντιστοίχιση, επιστρέφει στη κανονική ροή LLM.

Η αντιστοίχιση μονοπατιού είναι ακριβής σε σχέση με το ρυθμισμένο μονοπάτι trigger webhook.

Παράδειγμα:

- Μονοπάτι trigger στο SOP: `path = "/sop/deploy"`
- Αντίστοιχο αίτημα: `POST /sop/deploy`

### 3.2 Εξουσιοδότηση

Όταν είναι ενεργοποιημένο το pairing (προεπιλογή), παρέχετε:

1. `Authorization: Bearer <token>` (από `POST /pair`)
2. Προαιρετικό δεύτερο επίπεδο: `X-Webhook-Secret: <secret>` όταν είναι ρυθμισμένο webhook secret

### 3.3 Idempotency

Χρησιμοποιήστε:

`X-Idempotency-Key: <unique-key>`

Προεπιλογές:

- TTL: 300s
- Απόκριση διπλότυπου: `200 OK` με `"status": "duplicate"`

Τα κλειδιά idempotency είναι διαχωρισμένα ανά endpoint (`/webhook` vs `/sop/*`).

### 3.4 Παράδειγμα Αιτήματος

```bash
curl -X POST http://127.0.0.1:3000/sop/deploy \
  -H "Authorization: Bearer <token>" \
  -H "X-Idempotency-Key: $(uuidgen)" \
  -H "Content-Type: application/json" \
  -d '{"message":"deploy-service-a"}'
```

Τυπική απόκριση:

```json
{
  "status": "accepted",
  "matched_sops": ["deploy-pipeline"],
  "source": "sop_webhook",
  "path": "/sop/deploy"
}
```

## 4. Ενσωμάτωση Cron

Ο scheduler αξιολογεί τα αποθηκευμένα triggers cron χρησιμοποιώντας έλεγχο βασισμένο σε παράθυρο.

- **Βασισμένο σε παράθυρο:** τα συμβάντα εντός `(last_check, now]` δεν χάνονται.
- **Το πολύ μία φορά ανά έκφραση ανά tick:** αν πολλά σημεία εκκίνησης βρίσκονται σε ένα παράθυρο poll, η αποστολή γίνεται μία φορά.

Παράδειγμα trigger:

```toml
[[triggers]]
type = "cron"
expression = "0 0 8 * * *"
```

Οι εκφράσεις cron υποστηρίζουν 5, 6 ή 7 πεδία.

## 5. Προεπιλογές Ασφαλείας

| Χαρακτηριστικό | Μηχανισμός |
|---|---|
| **MQTT transport** | `mqtts://` + `use_tls = true` για TLS transport |
| **Εξουσιοδότηση Webhook** | Bearer token pairing (απαιτείται εξ ορισμού), προαιρετικό κοινόχρηστο secret header |
| **Rate limiting** | Όρια ανά client στις διαδρομές webhook (`webhook_rate_limit_per_minute`, προεπιλογή `60`) |
| **Idempotency** | Dedup βάσει header (`X-Idempotency-Key`, προεπιλεγμένο TTL `300s`) |
| **Επικύρωση Cron** | Μη έγκυρες εκφράσεις cron αποτυγχάνουν κατά την ανάλυση/κατασκευή cache |

## 6. Αντιμετώπιση Προβλημάτων

| Σύμπτωμα | Πιθανή Αιτία | Διόρθωση |
|---|---|---|
| Σφάλματα σύνδεσης **MQTT** | αναντιστοιχία broker URL/TLS | Επαληθεύστε ζεύγος scheme + TLS flag (`mqtt://`/`false`, `mqtts://`/`true`) |
| **Webhook** `401 Unauthorized` | λείπει bearer ή μη έγκυρο secret | επαναφέρετε token (`POST /pair`) και επαληθεύστε `X-Webhook-Secret` αν έχει ρυθμιστεί |
| **`/sop/*` επιστρέφει 404** | αναντιστοιχία μονοπατιού trigger | βεβαιωθείτε ότι το `SOP.toml` χρησιμοποιεί ακριβές μονοπάτι (π.χ. `/sop/deploy`) |
| **SOP ξεκίνησε αλλά το βήμα δεν εκτελέστηκε** | headless trigger χωρίς ενεργό agent loop | εκτελέστε agent loop για `ExecuteStep`, ή σχεδιάστε εκτέλεση για παύση σε εγκρίσεις |
| **Το Cron δεν εκκινεί** | daemon δεν εκτελείται ή μη έγκυρη έκφραση | εκτελέστε `zeroclaw daemon`· ελέγξτε logs για προειδοποιήσεις ανάλυσης cron |
