# Αναφορά Παρόχων ZeroClaw (Providers Reference)

Αυτό το έγγραφο περιγράφει τα ID των παρόχων, τα ψευδώνυμα (aliases) και τις μεταβλητές περιβάλλοντος για τη διαχείριση των διαπιστευτηρίων.

Τελευταία επαλήθευση: **21 Φεβρουαρίου 2026**.

## Προβολή Διαθέσιμων Παρόχων

Για να δείτε τη λίστα με τους ενεργούς παρόχους στο σύστημά σας, εκτελέστε:
```bash
zeroclaw providers
```

## Επίλυση Διαπιστευτηρίων (Credential Resolution)

Το runtime του ZeroClaw αναζητά διαπιστευτήρια με την εξής σειρά προτεραιότητας:

1. **Ρητές ρυθμίσεις**: Τιμές που έχουν οριστεί στο αρχείο `config.toml` ή μέσω παραμέτρων CLI.
2. **Μεταβλητές περιβάλλοντος παρόχου**: Μεταβλητές ειδικές για κάθε πάροχο (π.χ. `OPENAI_API_KEY`).
3. **Γενικές μεταβλητές**: Εφεδρικές μεταβλητές όπως οι `ZEROCLAW_API_KEY` ή `API_KEY`.

> [!NOTE]
> Σε περίπτωση χρήσης εφεδρικών παρόχων (`reliability.fallback_providers`), η επίλυση διαπιστευτηρίων γίνεται ανεξάρτητα για κάθε πάροχο. Τα κλειδιά του κύριου παρόχου δεν μεταφέρονται αυτόματα στους εφεδρικούς.

## Κατάλογος Παρόχων

| ID Παρόχου | Ψευδώνυμα | Τοπικός | Μεταβλητές Περιβάλλοντος |
|:---|:---|:---:|:---|
| `openrouter` | — | Όχι | `OPENROUTER_API_KEY` |
| `anthropic` | — | Όχι | `ANTHROPIC_API_KEY`, `ANTHROPIC_OAUTH_TOKEN` |
| `openai` | — | Όχι | `OPENAI_API_KEY` |
| `ollama` | — | Ναι | `OLLAMA_API_KEY` (προαιρετικό) |
| `gemini` | `google`, `google-gemini` | Όχι | `GEMINI_API_KEY`, `GOOGLE_API_KEY` |
| `bedrock` | `aws-bedrock` | Όχι | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` |
| `deepseek` | — | Όχι | `DEEPSEEK_API_KEY` |
| `mistral` | — | Όχι | `MISTRAL_API_KEY` |
| `groq` | — | Όχι | `GROQ_API_KEY` |
| `together` | `together-ai` | Όχι | `TOGETHER_API_KEY` |
| `fireworks` | `fireworks-ai` | Όχι | `FIREWORKS_API_KEY` |
| `perplexity` | — | Όχι | `PERPLEXITY_API_KEY` |
| `xai` | `grok` | Όχι | `XAI_API_KEY` |
| `cohere` | — | Όχι | `COHERE_API_KEY` |
| `ollama` | — | Ναι | - |
| `lmstudio` | `lm-studio` | Ναι | - |

## Ειδικές Σημειώσεις

### Gemini (Google)

- Υποστηρίζει έλεγχο ταυτότητας μέσω API Key ή OAuth (`~/.gemini/oauth_creds.json`).
- Τα μοντέλα συλλογιστικής (thinking models) υποστηρίζονται εγγενώς· το ZeroClaw φιλτράρει αυτόματα τα εσωτερικά metadata της συλλογιστικής.

### Ollama

- **Vision**: Υποστηρίζεται μέσω της σύνταξης `[IMAGE:<source>]` στα μηνύματα.
- **Cloud Routing**: Χρησιμοποιήστε το επίθεμα `:cloud` (π.χ. `llama3:cloud`) για απομακρυσμένα instances. Το `api_url` πρέπει να οριστεί ρητά.
- **Reasoning**: Η συμπεριφορά συλλογιστικής ελέγχεται μέσω της ρύθμισης `reasoning_enabled` στο αρχείο `config.toml`.

### AWS Bedrock

- Απαιτεί πλήρη διαπιστευτήρια AWS (Access Key ID και Secret Access Key).
- Χρησιμοποιεί το Converse API για τη διασφάλιση συμβατότητας με κλήσεις εργαλείων (tool calling).

## Προσαρμοσμένα Endpoints

Μπορείτε να ορίσετε παρόχους που ακολουθούν τα πρότυπα της αγοράς:
- **OpenAI-compatible**: `custom:https://your-api-url`
- **Anthropic-compatible**: `anthropic-custom:https://your-api-url`

## Δρομολόγηση Μοντέλων (Model Hints)

Χρησιμοποιήστε την ενότητα `[[model_routes]]` για να δημιουργήσετε σταθερά ψευδώνυμα για τα μοντέλα σας:

```toml
[[model_routes]]
hint = "fast"
provider = "groq"
model = "llama-3.3-70b-versatile"
```

Κλήση μέσω CLI: `zeroclaw agent --model hint:fast --message "..."`.

## Σχετική Τεκμηρίωση

- [config-reference.md](config-reference.md)
- [commands-reference.md](commands-reference.md)
- [custom-providers.md](custom-providers.md)
