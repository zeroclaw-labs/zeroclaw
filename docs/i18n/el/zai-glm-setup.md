# Διαμόρφωση Z.AI GLM

Το ZeroClaw υποστηρίζει τα μοντέλα GLM της Z.AI μέσω διεπαφών συμβατών με το OpenAI API.

---

## 1. Τελικά Σημεία και Ψευδώνυμα (Endpoints & Aliases)

Το ZeroClaw υποστηρίζει τα εξής προκαθορισμένα ψευδώνυμα για την Z.AI:

| Ψευδώνυμο | URL Τελικού Σημείου | Περιγραφή |
|:---|:---|:---|
| `zai` | `https://api.z.ai/api/coding/paas/v4` | Παγκόσμιο (Global) Endpoint |
| `zai-cn` | `https://open.bigmodel.cn/api/paas/v4` | Endpoint Κίνας |

> [!NOTE]
> Για τη χρήση προσαρμοσμένων διευθύνσεων (Custom Base URLs), συμβουλευτείτε τον οδηγό [custom-providers.md](custom-providers.md).

---

## 2. Διαδικασία Ρύθμισης

### Ταχεία Διαμόρφωση (Onboarding)

Χρησιμοποιήστε το CLI για αυτόματη ρύθμιση:
```bash
zeroclaw onboard \
  --provider "zai" \
  --api-key "YOUR_ZAI_API_KEY"
```

### Χειροκίνητη Διαμόρφωση

Επεξεργαστείτε το αρχείο `~/.zeroclaw/config.toml`:
```toml
api_key = "YOUR_ZAI_API_KEY"
default_provider = "zai"
default_model = "glm-5"
default_temperature = 0.7
```

---

## 3. Διαθέσιμα Μοντέλα

| Μοντέλο | Χαρακτηριστικά |
|:---|:---|
| `glm-5` | Κορυφαία απόδοση και προηγμένη συλλογιστική (Reasoning). |
| `glm-4.7` | Υψηλή ποιότητα για γενική χρήση. |
| `glm-4.5-air` | Βελτιστοποιημένο για χαμηλή καθυστέρηση (Low Latency). |

---

## 4. Επαλήθευση και Διάγνωση

### Δοκιμή Συνδεσιμότητας (Curl)

```bash
curl -X POST "https://api.z.ai/api/coding/paas/v4/chat/completions" \
  -H "Authorization: Bearer YOUR_ZAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "glm-5",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

### Μεταβλητές Περιβάλλοντος

Μπορείτε να ορίσετε το κλειδί στο αρχείο `.env`:
```bash
ZAI_API_KEY=id.secret # Μορφή: abc123.xyz789
```

---

## 5. Αντιμετώπιση Προβλημάτων

- **Περιορισμός Ρυθμού (Rate Limiting)**: Σε περίπτωση σφαλμάτων `rate_limited`, δοκιμάστε το μοντέλο `glm-4.5-air` ή ελέγξτε τα όρια του λογαριασμού σας στη Z.AI.
- **Σφάλμα Ελέγχου Ταυτότητας (401/403)**:
    - Επαληθεύστε τη μορφή `id.secret`.
    - Βεβαιωθείτε ότι δεν υπάρχουν περιττά κενά ή χαρακτήρες αλλαγής γραμμής στο κλειδί.
