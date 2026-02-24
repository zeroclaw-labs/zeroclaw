# Οδηγός Εγκατάστασης ZeroClaw σε Arduino Uno Q

Αυτός ο οδηγός περιγράφει τη διαδικασία εγκατάστασης και ρύθμισης του ZeroClaw στην πλευρά Linux του Arduino Uno Q.

## Επισκόπηση

Το ZeroClaw παρέχει πλήρη υποστήριξη για το Arduino Uno Q χωρίς να απαιτούνται αλλαγές στον κώδικα.

| Στοιχείο | Τοποθεσία | Περιγραφή |
|:---|:---|:---|
| Εφαρμογή Bridge | `firmware/zeroclaw-uno-q-bridge/` | MCU sketch και Python socket server για τη διαχείριση των GPIO. |
| Εργαλεία Bridge | `src/peripherals/uno_q_bridge.rs` | Εργαλεία `gpio_read` / `gpio_write` για επικοινωνία μέσω TCP. |
| Εντολή Setup | `src/peripherals/uno_q_setup.rs` | Η εντολή `zeroclaw peripheral setup-uno-q` για την ανάπτυξη του Bridge. |

> **Σημείωση**: Απαιτείται μεταγλώττιση (build) με το feature `hardware` για την υποστήριξη του Uno Q.

## Προαπαιτούμενα

- Arduino Uno Q με ενεργή σύνδεση WiFi.
- Εγκατεστημένο Arduino App Lab για την αρχική προετοιμασία.
- Κλειδί API για πάροχο LLM (π.χ. OpenRouter, Gemini).

---

## Βήμα 1: Αρχική Προετοιμασία Uno Q

### 1.1 Ρύθμιση μέσω App Lab

1. Εκκινήστε το **Arduino App Lab**.
2. Συνδέστε το Uno Q μέσω USB και ενεργοποιήστε τη συσκευή.
3. Συνδεθείτε στην πλακέτα και ακολουθήστε τις οδηγίες:
   - Ορίστε διαπιστευτήρια SSH (Όνομα χρήστη και κωδικό πρόσβασης).
   - Ρυθμίστε τη σύνδεση WiFi.
   - Ενημερώστε το υλικολογισμικό (firmware) εάν απαιτείται.
4. Σημειώστε τη διεύθυνση IP της συσκευής (π.χ. `192.168.1.42`).

### 1.2 Επαλήθευση Πρόσβασης SSH

Επιβεβαιώστε τη σύνδεση μέσω τερματικού:
```bash
ssh arduino@<UNO_Q_IP>
```

---

## Βήμα 2: Εγκατάσταση του ZeroClaw

### Μεταγλώττιση στη Συσκευή (Προτεινόμενο)

1. **Σύνδεση μέσω SSH**:
   ```bash
   ssh arduino@<UNO_Q_IP>
   ```

2. **Εγκατάσταση Rust**:
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
   source ~/.cargo/env
   ```

3. **Εγκατάσταση Εξαρτήσεων**:
   ```bash
   sudo apt-get update
   sudo apt-get install -y pkg-config libssl-dev git
   ```

4. **Λήψη και Μεταγλώττιση**:
   ```bash
   git clone https://github.com/theonlyhennygod/zeroclaw.git
   cd zeroclaw
   cargo build --release --features hardware
   ```

5. **Εγκατάσταση Εκτελέσιμου**:
   ```bash
   sudo cp target/release/zeroclaw /usr/local/bin/
   ```

---

## Βήμα 3: Ρύθμιση του ZeroClaw

### 3.1 Αυτόματη Προετοιμασία (Onboarding)

```bash
zeroclaw onboard --api-key <YOUR_API_KEY> --provider <provider_name>
```

### 3.2 Αρχείο Ρυθμίσεων (config.toml)

Βεβαιωθείτε ότι το αρχείο `~/.zeroclaw/config.toml` περιλαμβάνει τις απαραίτητες ρυθμίσεις για το Telegram και τον πράκτορα.

---

## Βήμα 4: Εκτέλεση του ZeroClaw Daemon

Ξεκινήστε την υπηρεσία ZeroClaw:
```bash
zeroclaw daemon --host 127.0.0.1 --port 42617
```
Σε αυτό το στάδιο, η επικοινωνία μέσω Telegram είναι ενεργή, αλλά χωρίς έλεγχο των GPIO.

---

## Βήμα 5: Ενεργοποίηση GPIO μέσω Bridge

### 5.1 Ανάπτυξη της Εφαρμογής Bridge

Από τον υπολογιστή σας ή απευθείας από το Uno Q, εκτελέστε:
```bash
zeroclaw peripheral setup-uno-q --host <UNO_Q_IP>
```

### 5.2 Ενημέρωση Ρυθμίσεων

Προσθέστε τα παρακάτω στο `config.toml`:

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "arduino-uno-q"
transport = "bridge"
```

### 5.3 Επανεκκίνηση

Επανεκκινήστε τον daemon:
```bash
zeroclaw daemon --host 127.0.0.1 --port 42617
```

---

## Αντιμετώπιση Προβλημάτων

- **Σφάλμα "command not found"**: Βεβαιωθείτε ότι η διαδρομή `/usr/local/bin` ή `~/.cargo/bin` περιλαμβάνεται στη μεταβλητή `PATH`.
- **Το Telegram δεν αποκρίνεται**: Επαληθεύστε το `bot_token`, τη λίστα `allowed_users` και τη σύνδεση WiFi του Uno Q.
- **Έλλειψη Μνήμης (OOM)**: Χρησιμοποιήστε μόνο τα απαραίτητα features κατά το build και ενεργοποιήστε τη ρύθμιση `compact_context = true` στις ρυθμίσεις του πράκτορα.
- **Προβλήματα GPIO**: Βεβαιωθείτε ότι η εφαρμογή Bridge εκτελείται και ότι η ρύθμιση `transport` είναι ορισμένη σε `bridge`.
