# Αντιμετώπιση Προβλημάτων (Troubleshooting)

Αυτός ο οδηγός περιγράφει λύσεις για κοινά ζητήματα εγκατάστασης και εκτέλεσης του ZeroClaw.

Τελευταία ενημέρωση: 20 Φεβρουαρίου 2026.

---

## 1. Εγκατάσταση και Προετοιμασία (Bootstrap)

### Σφάλμα: `cargo is not installed`

**Αιτία**: Η Rust toolchain δεν είναι εγκατεστημένη.
**Λύση**:
Εκτελέστε την αυτόματη εγκατάσταση:
```bash
./bootstrap.sh --install-rust
```
Εναλλακτικά, επισκεφθείτε τη διεύθυνση [rustup.rs](https://rustup.rs/).

### Σφάλματα Μεταγλώττισης (Compilation Errors)

**Σύμπτωμα**: Αποτυχία λόγω προβλημάτων στον μεταγλωττιστή ή στο `pkg-config`.
**Λύση**:
Εγκαταστήστε τις εξαρτήσεις συστήματος (system dependencies):
```bash
./bootstrap.sh --install-system-deps
```

### Περιορισμένοι Πόροι (RAM / Disk Space)

**Σύμπτωμα**: Τερματισμός της διαδικασίας από τον OOM killer ή σφάλμα `cannot allocate memory`.
**Λύση**:
Χρησιμοποιήστε προ-μεταγλωττισμένα (prebuilt) αρχεία:
```bash
./bootstrap.sh --prefer-prebuilt
```
Εάν επιθυμείτε μεταγλώττιση από τον πηγαίο κώδικα σε περιβάλλον με περιορισμένη μνήμη, περιορίστε τον παραλληλισμό:
```bash
CARGO_BUILD_JOBS=1 cargo build --release --locked
```

---

## 2. Χρόνος Εκκίνησης και Συνδεσιμότητα (Runtime)

### Αργή Μεταγλώττιση

Η στοίβα Matrix E2EE και τα native scripts για κρυπτογραφία απαιτούν σημαντικούς πόρους.
- Για ταχύτερο τοπικό έλεγχο χωρίς Matrix:
  ```bash
  cargo check
  ```
- Για ανάλυση χρόνων μεταγλώττισης:
  ```bash
  cargo check --timings
  ```

### Η Πύλη (Gateway) δεν είναι προσβάσιμη

Επαληθεύστε την κατάσταση του συστήματος:
```bash
zeroclaw status
zeroclaw doctor
```
Ελέγξτε τις ρυθμίσεις στο `~/.zeroclaw/config.toml`:
- `[gateway].host` (Προεπιλογή: `127.0.0.1`)
- `[gateway].port` (Προεπιλογή: `42617`)

---

## 3. Κανάλια Επικοινωνίας (Channels)

### Σύγκρουση Telegram: `terminated by other getUpdates request`

**Αιτία**: Πολλαπλά instances χρησιμοποιούν το ίδιο Bot Token.
**Λύση**: Τερματίστε όλες τις άλλες διεργασίες που χρησιμοποιούν το συγκεκριμένο token.

### Διάγνωση Καναλιού

Εκτελέστε την εντολή:
```bash
zeroclaw channel doctor
```

---

## 4. Λειτουργία ως Υπηρεσία (Service Mode)

### Η υπηρεσία δεν εκκινεί

Ελέγξτε την κατάσταση μέσω του ZeroClaw CLI:
```bash
zeroclaw service status
```
Για προβολή των logs στο Linux:
```bash
journalctl --user -u zeroclaw.service -f
```

---

## 5. Υποβολή Αναφοράς Προβλήματος

Εάν το πρόβλημα επιμένει, συμπεριλάβετε τα αποτελέσματα των παρακάτω εντολών στην αναφορά σας:
```bash
zeroclaw --version
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
```

> [!TIP]
> Παρακαλείστε να αφαιρέσετε τυχόν ευαίσθητα δεδομένα (API keys, Tokens) από το αρχείο ρυθμίσεων πριν το κοινοποιήσετε.
