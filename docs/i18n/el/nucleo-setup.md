# ZeroClaw σε Nucleo-F401RE — Οδηγός Βήμα προς Βήμα

Εκτελέστε το ZeroClaw στον κεντρικό υπολογιστή σας (Mac ή Linux). Συνδέστε ένα Nucleo-F401RE μέσω USB. Ελέγξτε τα GPIO (LED, ακίδες) μέσω Telegram ή CLI.

---

## Λήψη πληροφοριών πλακέτας μέσω Telegram (Χωρίς ανάγκη για υλικολογισμικό)

Το ZeroClaw μπορεί να διαβάσει πληροφορίες για το τσιπ από το Nucleo μέσω USB **χωρίς να προγραμματίσετε κανένα υλικολογισμικό**. Στείλτε μήνυμα στο bot σας στο Telegram:

- *"Τι πληροφορίες πλακέτας έχω;"*
- *"Πληροφορίες πλακέτας"*
- *"Ποιο υλικό είναι συνδεδεμένο;"*
- *"Chip info"*

Ο πράκτορας χρησιμοποιεί το εργαλείο `hardware_board_info` για να επιστρέψει το όνομα του τσιπ, την αρχιτεκτονική και τον χάρτη μνήμης. Με τη λειτουργία `probe`, διαβάζει ζωντανά δεδομένα μέσω USB/SWD· διαφορετικά, επιστρέφει στατικές πληροφορίες από το φύλλο δεδομένων.

**Ρύθμιση:** Προσθέστε πρώτα το Nucleo στο αρχείο `config.toml` (ώστε ο πράκτορας να γνωρίζει ποια πλακέτα να αναζητήσει):

```toml
[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200
```

**Εναλλακτική μέσω CLI:**

```bash
cargo build --features hardware,probe
zeroclaw hardware info
zeroclaw hardware discover
```

---

## Τι περιλαμβάνεται (Δεν απαιτούνται αλλαγές στον κώδικα)

Το ZeroClaw περιλαμβάνει όλα τα απαραίτητα για το Nucleo-F401RE:

| Συστατικό | Τοποθεσία | Σκοπός |
|-----------|----------|---------|
| Υλικολογισμικό (Firmware) | `firmware/zeroclaw-nucleo/` | Embassy Rust — USART2 (115200), gpio_read, gpio_write |
| Σειριακό περιφερειακό | `src/peripherals/serial.rs` | Πρωτόκολλο JSON μέσω σειριακής (όπως στο Arduino/ESP32) |
| Εντολή προγραμματισμού (Flash) | `zeroclaw peripheral flash-nucleo` | Κατασκευάζει το υλικολογισμικό και το προγραμματίζει μέσω probe-rs |

Πρωτόκολλο: JSON οριοθετημένο με νέα γραμμή. Αίτημα: `{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}`. Απόκριση: `{"id":"1","ok":true,"result":"done"}`.

---

## Προαπαιτούμενα

- Πλακέτα Nucleo-F401RE.
- Καλώδιο USB (USB-A σε Mini-USB· το Nucleo έχει ενσωματωμένο ST-Link).
- Για τον προγραμματισμό (flashing): `cargo install probe-rs-tools --locked` (ή χρησιμοποιήστε το [σενάριο εγκατάστασης](https://probe.rs/docs/getting-started/installation/)).

---

## Φάση 1: Προγραμματισμός Υλικολογισμικού (Flash)

### 1.1 Σύνδεση Nucleo

1. Συνδέστε το Nucleo στον κεντρικό υπολογιστή σας (Mac/Linux) μέσω USB.
2. Η πλακέτα εμφανίζεται ως συσκευή USB (ST-Link). Δεν απαιτείται ξεχωριστός οδηγός στα σύγχρονα συστήματα.

### 1.2 Προγραμματισμός μέσω ZeroClaw

Από τον ριζικό κατάλογο του ZeroClaw:

```bash
zeroclaw peripheral flash-nucleo
```

Αυτό κατασκευάζει το `firmware/zeroclaw-nucleo` και εκτελεί την εντολή `probe-rs run --chip STM32F401RETx`. Το υλικολογισμικό εκτελείται αμέσως μετά τον προγραμματισμό.

### 1.3 Χειροκίνητος Προγραμματισμός (Εναλλακτική)

```bash
cd firmware/zeroclaw-nucleo
cargo build --release --target thumbv7em-none-eabihf
probe-rs run --chip STM32F401RETx target/thumbv7em-none-eabihf/release/zeroclaw-nucleo
```

---

## Φάση 2: Εύρεση της Σειριακής Θύρας

- **macOS:** `/dev/cu.usbmodem*` ή `/dev/tty.usbmodem*` (π.χ. `/dev/cu.usbmodem101`).
- **Linux:** `/dev/ttyACM0` (ή ελέγξτε το `dmesg` μετά τη σύνδεση).

Το USART2 (PA2/PA3) είναι γεφυρωμένο με την εικονική θύρα COM του ST-Link, οπότε ο κεντρικός υπολογιστής βλέπει μία σειριακή συσκευή.

---

## Φάση 3: Ρύθμιση του ZeroClaw

Προσθέστε στο αρχείο `~/.zeroclaw/config.toml`:

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/cu.usbmodem101"   # προσαρμόστε στη θύρα σας
baud = 115200
```

---

## Φάση 4: Εκτέλεση και Δοκιμή

```bash
zeroclaw daemon --host 127.0.0.1 --port 42617
```

Ή χρησιμοποιήστε τον πράκτορα απευθείας:

```bash
zeroclaw agent --message "Turn on the LED on pin 13"
```

Ακίδα (Pin) 13 = PA5 = LED Χρήστη (LD2) στο Nucleo-F401RE.

---

## Σύνοψη: Εντολές

| Βήμα | Εντολή |
|------|---------|
| 1 | Συνδέστε το Nucleo μέσω USB |
| 2 | `cargo install probe-rs-tools --locked` |
| 3 | `zeroclaw peripheral flash-nucleo` |
| 4 | Προσθέστε το Nucleo στο config.toml (διαδρομή = η σειριακή σας θύρα) |
| 5 | `zeroclaw daemon` ή `zeroclaw agent -m "Turn on LED"` |

---

## Αντιμετώπιση Προβλημάτων

- **flash-nucleo unrecognized**: Κατασκευάστε από το αποθετήριο: `cargo run --features hardware -- peripheral flash-nucleo`. Η υποεντολή υπάρχει μόνο στην κατασκευή από το αποθετήριο.
- **probe-rs not found**: `cargo install probe-rs-tools --locked` (το crate `probe-rs` είναι βιβλιοθήκη· το CLI βρίσκεται στο `probe-rs-tools`).
- **No probe detected**: Βεβαιωθείτε ότι το Nucleo είναι συνδεδεμένο. Δοκιμάστε άλλο καλώδιο ή θύρα USB.
- **Serial port not found**: Στο Linux, προσθέστε τον χρήστη στην ομάδα `dialout`: `sudo usermod -a -G dialout $USER`.
- **GPIO commands ignored**: Ελέγξτε αν η διαδρομή (`path`) στις ρυθμίσεις αντιστοιχεί στη σειριακή σας θύρα. Εκτελέστε `zeroclaw peripheral list` για επαλήθευση.
