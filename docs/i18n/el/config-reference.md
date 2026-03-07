# Οδηγός Ρυθμίσεων ZeroClaw (config.toml)

Αυτός ο οδηγός εξηγεί τις πιο σημαντικές ρυθμίσεις που μπορείτε να κάνετε στο αρχείο `config.toml`.

Τελευταίος έλεγχος: **19 Φεβρουαρίου 2026**.

## Πού βρίσκεται το αρχείο ρυθμίσεων;

Το ZeroClaw ψάχνει για τις ρυθμίσεις με την εξής σειρά:
1. Στη διαδρομή που ορίζει η μεταβλητή `ZEROCLAW_WORKSPACE`.
2. Στο αρχείο `~/.zeroclaw/config.toml` (αυτή είναι η συνηθισμένη θέση).

## Βασικές Ρυθμίσεις (Core)

| Ρύθμιση | Τι ορίζει |
|---|---|
| `default_provider` | Ποιον πάροχο AI χρησιμοποιείτε (π.χ. `openai`, `ollama`). |
| `default_model` | Ποιο συγκεκριμένο μοντέλο AI χρησιμοποιείτε (π.χ. `gpt-4o`). |
| `default_temperature` | Πόσο "δημιουργική" θα είναι η AI (τιμή από 0 έως 2). |

## 1. Συμπεριφορά της AI (Agent)

- `max_tool_iterations`: Πόσες φορές μπορεί η AI να χρησιμοποιήσει εργαλεία για να απαντήσει σε 1 μήνυμα (προεπιλογή: 10).
- `max_history_messages`: Πόσα προηγούμενα μηνύματα θυμάται η AI στη συνομιλία (προεπιλογή: 50).

## 2. Αυτονομία και Ασφάλεια (Autonomy)

Εδώ ρυθμίζετε πόση ελευθερία έχει η AI να κάνει αλλαγές στον υπολογιστή σας.

- `level`: 
    - `read_only`: Μπορεί μόνο να διαβάζει αρχεία.
    - `supervised`: Χρειάζεται την έγκρισή σας για σημαντικές ενέργειες (προεπιλογή).
    - `full`: Μπορεί να τρέχει εντολές ελεύθερα (προσοχή!).
- `allowed_commands`: Λίστα με τις εντολές που επιτρέπεται να τρέχει η AI.
- `forbidden_paths`: Φάκελοι που η AI **δεν** επιτρέπεται να αγγίξει (π.χ. `/etc`).

## 3. Μνήμη (Memory)

Πώς αποθηκεύει η AI τις πληροφορίες που της δίνετε.
- `backend`: Μπορεί να είναι `sqlite` (βάση δεδομένων), `markdown` (απλά αρχεία κειμένου) ή `none` (καμία μνήμη).

## 4. Κανάλια Επικοινωνίας (Channels)

Κάθε κανάλι (Telegram, Discord κ.λπ.) έχει τη δική του ενότητα στο αρχείο.

Παράδειγμα για το **Telegram**:
```toml
[channels_config.telegram]
bot_token = "το-κλειδί-σας"
allowed_users = ["το-όνομά-σας"] # Ποιοι επιτρέπεται να μιλάνε στο bot
```

## 5. Έλεγχος Κόστους (Cost)

Αν χρησιμοποιείτε πληρωμένες υπηρεσίες AI, μπορείτε να βάλετε όρια.
- `daily_limit_usd`: Μέγιστο ποσό ανά ημέρα (π.χ. 10.00 δολάρια).
- `monthly_limit_usd`: Μέγιστο ποσό ανά μήνα.

## 6. Εικόνες (Multimodal)

Ρυθμίσεις για το πώς η AI βλέπει εικόνες.
- `max_images`: Μέγιστος αριθμός εικόνων ανά μήνυμα.
- `allow_remote_fetch`: Αν επιτρέπεται στην AI να κατεβάζει εικόνες από το ίντερνετ μέσω συνδέσμων (links).

---

## Συμβουλές

- Αν αλλάξετε το αρχείο `config.toml`, πρέπει να κάνετε επανεκκίνηση το ZeroClaw για να δει τις αλλαγές.
- Χρησιμοποιήστε την εντολή `zeroclaw doctor` για να βεβαιωθείτε ότι οι ρυθμίσεις σας είναι σωστές.

## Ενημέρωση (2026-03-03)

- Στην ενότητα `[agent]` προστέθηκαν τα `allowed_tools` και `denied_tools`.
  - Αν το `allowed_tools` δεν είναι κενό, ο primary agent βλέπει μόνο τα εργαλεία της λίστας.
  - Το `denied_tools` εφαρμόζεται μετά το allowlist και αφαιρεί επιπλέον εργαλεία.
- Άγνωστες τιμές στο `allowed_tools` αγνοούνται (με debug log) και δεν μπλοκάρουν την εκκίνηση.
- Αν `allowed_tools` και `denied_tools` καταλήξουν να αφαιρέσουν όλα τα εκτελέσιμα εργαλεία, η εκκίνηση αποτυγχάνει άμεσα με σαφές μήνυμα ρύθμισης.
- Για πλήρη πίνακα πεδίων και παράδειγμα, δείτε το αγγλικό `config-reference.md` στην ενότητα `[agent]`.
- Μην μοιράζεστε ποτέ το αρχείο `config.toml` με άλλους, καθώς περιέχει τα μυστικά κλειδιά σας (tokens).
- Νέα ρύθμιση παρατηρησιμότητας: `observability.runtime_trace_record_http` (προεπιλογή `false`) για καταγραφή λεπτομερειών HTTP κλήσεων LLM (`llm_http_request` / `llm_http_response`). Ενεργό μόνο όταν `runtime_trace_mode` είναι `rolling` ή `full`. Τα payloads αποκρύπτουν ευαίσθητα πεδία, αλλά τα αρχεία trace παραμένουν ευαίσθητα δεδομένα λειτουργίας. Τα αιτήματα/απαντήσεις/headers περαιρούνονται αν είναι πολύ μεγάλα. Σκεφτείτε να απενεργοποιήσετε HTTP tracing σε παραγωγή.

## 7. Παρατηρησιμότητα (Observability)

Ρυθμίσεις για παρακολούθηση και εντοπισμό προβλημάτων.

### `[observability]`

| Ρύθμιση | Προεπιλογή | Σκοπός |
|---|---|---|
| `backend` | `none` | Υπόστρωμα παρατηρησιμότητας: `none`, `noop`, `log`, `prometheus`, `otel`, `opentelemetry` ή `otlp` |
| `otel_endpoint` | `http://localhost:4318` | OTLP HTTP endpoint όταν το υπόστρωμα είναι `otel` |
| `otel_service_name` | `zeroclaw` | Όνομα υπηρεσίας που αποστέλλεται στον συλλέκτη OTLP |
| `runtime_trace_mode` | `none` | Λειτουργία αποθήκευσης runtime trace: `none`, `rolling` ή `full` |
| `runtime_trace_path` | `state/runtime-trace.jsonl` | Διαδρομή JSONL runtime trace (σχετικά με workspace εκτός αν είναι απόλυτη) |
| `runtime_trace_max_entries` | `200` | Μέγιστα αποθηκευμένα γεγονότα όταν `runtime_trace_mode = "rolling"` |
| `runtime_trace_record_http` | `false` | Καταγραφή λεπτομερών HTTP αιτημάτων/απαντήσεων LLM (`llm_http_request` / `llm_http_response`) στο runtime trace |

Σημειώσεις:

- `backend = "otel"` χρησιμοποιεί OTLP HTTP export με blocking exporter client ώστε span και metric να μπορούν να αποστέλλονται με ασφάλεια από περιβάλλοντα non-Tokio.
- Οι τιμές alias `opentelemetry` και `otlp` αντιστοιχούν στο ίδιο υπόστρωμα OTel.
- Τα runtime traces προορίζονται για εντοπισμό tool-call αποτυχιών και κακοσχηματισμένων tool payload του μοντέλου. Μπορούν να περιέχουν κείμενο εξόδου μοντέλου, οπότε κρατήστε τα απενεργοποιημένα από προεπιλογή σε κοινόχρηστους hosts.
- `runtime_trace_record_http` είναι ενεργό μόνο όταν `runtime_trace_mode` είναι `rolling` ή `full`.
  - Τα payloads HTTP trace αποκρύπτουν κοινά ευαίσθητα πεδία (π.χ. headers Authorization και πεδία query/body τύπου token), αλλά αντιμετωπίστε τα αρχεία trace ως ευαίσθητα δεδομένα λειτουργίας.
  - Για streaming αιτήματα, για βελτίωση αποδοτικότητας, η καταγραφή του σώματος απόκρισης παραλείπεται, ενώ τα σώματα αιτημάτων συνεχίζουν να καταγράφονται (εντός ορίων μεγέθους).
  - Οι τιμές αιτημάτων/απαντήσεων/header περικόπτονται αν είναι πολύ μεγάλες. Ωστόσο, LLM κυκλοφορία υψηλού όγκου με μεγάλες απαντήσεις μπορεί ακόμα να αυξήσει σημαντικά τη χρήση μνήμης και το μέγεθος αρχείων trace.
  - Σκεφτείτε να απενεργοποιήσετε HTTP tracing σε περιβάλλοντα παραγωγής.
- Αναζήτηση runtime traces με:
  - `zeroclaw doctor traces --limit 20`
  - `zeroclaw doctor traces --event tool_call_result --contains \"error\"`
  - `zeroclaw doctor traces --event llm_http_response --contains \"500\"`
  - `zeroclaw doctor traces --id <trace-id>`

Παράδειγμα:

```toml
[observability]
backend = "otel"
otel_endpoint = "http://localhost:4318"
otel_service_name = "zeroclaw"
runtime_trace_mode = "rolling"
runtime_trace_path = "state/runtime-trace.jsonl"
runtime_trace_max_entries = 200
runtime_trace_record_http = true
```
