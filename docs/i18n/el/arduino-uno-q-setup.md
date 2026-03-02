# ZeroClaw σε Arduino Uno Q — Οδηγός βήμα προς βήμα

Εκτελέστε το ZeroClaw στην πλευρά Linux του Arduino Uno Q. Το Telegram λειτουργεί μέσω WiFi· ο έλεγχος GPIO χρησιμοποιεί το Bridge (απαιτεί μια ελάχιστη εφαρμογή App Lab).

---

## Τι περιλαμβάνεται (Δεν απαιτούνται αλλαγές κώδικα)

Το ZeroClaw περιλαμβάνει όλα όσα χρειάζονται για το Arduino Uno Q. **Κλωνοποιήστε το repo και ακολουθήστε αυτόν τον οδηγό — δεν χρειάζονται patches ή προσαρμοσμένος κώδικας.**

| Στοιχείο | Τοποθεσία | Σκοπός |
|-----------|----------|---------|
| Εφαρμογή Bridge | `firmware/zeroclaw-uno-q-bridge/` | MCU sketch + Python socket server (port 9999) για GPIO |
| Εργαλεία Bridge | `src/peripherals/uno_q_bridge.rs` | Εργαλεία `gpio_read` / `gpio_write` που επικοινωνούν με το Bridge μέσω TCP |
| Εντολή Setup | `src/peripherals/uno_q_setup.rs` | `zeroclaw peripheral setup-uno-q` αναπτύσσει το Bridge μέσω scp + arduino-app-cli |
| Σχήμα Config | `board = "arduino-uno-q"`, `transport = "bridge"` | Υποστηρίζεται στο `config.toml` |

Κάντε build με `--features hardware` για να συμπεριλάβετε την υποστήριξη Uno Q.

---

## Προαπαιτούμενα

- Arduino Uno Q με WiFi ρυθμισμένο
- Arduino App Lab εγκατεστημένο στον υπολογιστή σας (για αρχική ρύθμιση και ανάπτυξη)
- Κλειδί API για LLM (OpenRouter, κ.λπ.)

---

## Φάση 1: Αρχική ρύθμιση Uno Q (Μία φορά)

### 1.1 Ρυθμίστε το Uno Q μέσω App Lab

1. Κατεβάστε το [Arduino App Lab](https://docs.arduino.cc/software/app-lab/) (αρχείο tar.gz στο Linux).
2. Συνδέστε το Uno Q μέσω USB, ενεργοποιήστε το.
3. Ανοίξτε το App Lab, συνδεθείτε με την πλακέτα.
4. Ακολουθήστε τον οδηγό ρύθμισης:
   - Ορίστε όνομα χρήστη και κωδικό πρόσβασης (για SSH)
   - Ρυθμίστε το WiFi (SSID, κωδικό πρόσβασης)
   - Εφαρμόστε ενημερώσεις firmware
5. Σημειώστε τη διεύθυνση IP που εμφανίζεται (π.χ. `arduino@192.168.1.42`) ή βρείτε τη αργότερα μέσω `ip addr show` στο τερματικό του App Lab.

### 1.2 Επαληθεύστε την πρόσβαση SSH

```bash
ssh arduino@<UNO_Q_IP>
# Εισάγετε τον κωδικό πρόσβασης που ορίσατε
```

---

## Φάση 2: Εγκατάστειλη του ZeroClaw στο Uno Q

### Επιλογή A: Build στη Συσκευή (Απλούστερο, ~20–40 λεπτά)

```bash
# SSH στο Uno Q
ssh arduino@<UNO_Q_IP>

# Εγκαταστήστε το Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# Εγκαταστήστε τις εξαρτήσεις build (Debian)
sudo apt-get update
sudo apt-get install -y pkg-config libssl-dev

# Κλωνοποιήστε το zeroclaw (ή scp το project σας)
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# Build (~15–30 λεπτά στο Uno Q)
cargo build --release --features hardware

# Εγκατάστειλη
sudo cp target/release/zeroclaw /usr/local/bin/
```

### Επιλογή B: Cross-Compile στο Mac (Ταχύτερο)

```bash
# Στο Mac σας — προσθέστε το στόχο aarch64
rustup target add aarch64-unknown-linux-gnu

# Εγκαταστήστε τον cross-compiler (macOS; απαιτείται για linking)
brew tap messense/macos-cross-toolchains
brew install aarch64-unknown-linux-gnu

# Build
CC_aarch64_unknown_linux_gnu=aarch64-unknown-linux-gnu-gcc cargo build --release --target aarch64-unknown-linux-gnu --features hardware

# Αντιγράψτε στο Uno Q
scp target/aarch64-unknown-linux-gnu/release/zeroclaw arduino@<UNO_Q_IP>:~/
ssh arduino@<UNO_Q_IP> "sudo mv ~/zeroclaw /usr/local/bin/"
```

Εάν το cross-compile αποτύχει, χρησιμοποιήστε την Επιλογή A και κάντε build στη συσκευή.

---

## Φάση 3: Ρυθμίστε το ZeroClaw

### 3.1 Εκτελέστε Onboard (ή δημιουργήστε Config χειροκίνητα)

```bash
ssh arduino@<UNO_Q_IP>

# Γρήγορη ρύθμιση
zeroclaw onboard --api-key YOUR_OPENROUTER_KEY --provider openrouter

# Ή δημιουργήστε config χειροκίνητα
mkdir -p ~/.zeroclaw/workspace
nano ~/.zeroclaw/config.toml
```

### 3.2 Ελάχιστο config.toml

```toml
api_key = "YOUR_OPENROUTER_API_KEY"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"

[peripherals]
enabled = false
# GPIO μέσω Bridge απαιτεί τη Φάση 4

[channels_config.telegram]
bot_token = "YOUR_TELEGRAM_BOT_TOKEN"
allowed_users = ["*"]

[gateway]
host = "127.0.0.1"
port = 42617
allow_public_bind = false

[agent]
compact_context = true
```

---

## Φάση 4: Εκτελέστε τον ZeroClaw Daemon

```bash
ssh arduino@<UNO_Q_IP>

# Εκτελέστε τον daemon (Telegram polling λειτουργεί μέσω WiFi)
zeroclaw daemon --host 127.0.0.1 --port 42617
```

**Σε αυτό το σημείο:** Το Telegram chat λειτουργεί. Στείλτε μηνύματα στο bot δικό σας — το ZeroClaw απαντά. Ακόμα χωρίς GPIO.

---

## Φάση 5: GPIO μέσω Bridge (Το ZeroClaw το χειρίζεται)

Το ZeroClaw περιλαμβάνει την εφαρμογή Bridge και την εντολή setup.

### 5.1 Ανάπτυξη της εφαρμογής Bridge

**Από το Mac σας** (με zeroclaw repo):
```bash
zeroclaw peripheral setup-uno-q --host 192.168.0.48
```

**Από το Uno Q** (SSH'd in):
```bash
zeroclaw peripheral setup-uno-q
```

Αυτό αντιγράφει την εφαρμογή Bridge στο `~/ArduinoApps/zeroclaw-uno-q-bridge` και την ξεκινά.

### 5.2 Προσθέστε στο config.toml

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "arduino-uno-q"
transport = "bridge"
```

### 5.3 Εκτελέστε το ZeroClaw

```bash
zeroclaw daemon --host 127.0.0.1 --port 42617
```

Τώρα όταν στείλετε μήνυμα στο Telegram bot δικό σας *"Turn on the LED"* ή *"Set pin 13 high"*, το ZeroClaw χρησιμοποιεί `gpio_write` μέσω του Bridge.

---

## Περίληψη: Εντολές από αρχή έως τέλος

| Βήμα | Εντολή |
|------|---------|
| 1 | Ρυθμίστε το Uno Q στο App Lab (WiFi, SSH) |
| 2 | `ssh arduino@<IP>` |
| 3 | `curl -sSf https://sh.rustup.rs \| sh -s -- -y && source ~/.cargo/env` |
| 4 | `sudo apt-get install -y pkg-config libssl-dev` |
| 5 | `git clone https://github.com/zeroclaw-labs/zeroclaw.git && cd zeroclaw` |
| 6 | `cargo build --release --features hardware` |
| 7 | `zeroclaw onboard --api-key KEY --provider openrouter` |
| 8 | Επεξεργασία `~/.zeroclaw/config.toml` (προσθήκη Telegram bot_token) |
| 9 | `zeroclaw daemon --host 127.0.0.1 --port 42617` |
| 10 | Στείλτε μήνυμα στο Telegram bot δικό σας — απαντά |

---

## Αντιμετώπιση προβλημάτων

- **"command not found: zeroclaw"** — Χρησιμοποιήστε τη πλήρη διαδρομή: `/usr/local/bin/zeroclaw` ή βεβαιωθείτε ότι το `~/.cargo/bin` είναι στο PATH.
- **Το Telegram δεν απαντα** — Ελέγξτε το bot_token, allowed_users και ότι το Uno Q έχει internet (WiFi).
- **Out of memory** — Κρατήστε τα features ελάχιστα (`--features hardware` για Uno Q); εξετάστε `compact_context = true`.
- **Εντολές GPIO αγνοούνται** — Βεβαιωθείτε ότι η εφαρμογή Bridge εκτελείται (`zeroclaw peripheral setup-uno-q` την αναπτύσσει και ξεκινά). Το Config πρέπει να έχει `board = "arduino-uno-q"` και `transport = "bridge"`.
- **Πάροχος LLM (GLM/Zhipu)** — Χρησιμοποιήστε `default_provider = "glm"` ή `"zhipu"` με `GLM_API_KEY` στο env ή config. Το ZeroClaw χρησιμοποιεί το σωστό endpoint v4.
