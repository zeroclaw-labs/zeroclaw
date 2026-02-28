# Ανάπτυξη Δικτύου — ZeroClaw σε Raspberry Pi και Τοπικό Δίκτυο

Αυτό το έγγραφο καλύπτει την ανάπτυξη του ZeroClaw σε ένα Raspberry Pi ή σε άλλον κεντρικό υπολογιστή στο τοπικό σας δίκτυο, με κανάλια Telegram και προαιρετικά κανάλια webhook.

---

## 1. Επισκόπηση

| Λειτουργία | Απαιτείται εισερχόμενη θύρα; | Περίπτωση χρήσης |
|------|----------------------|----------|
| **Telegram polling** | Όχι | Το ZeroClaw αντλεί δεδομένα από το API του Telegram. Λειτουργεί από παντού. |
| **Matrix sync (συμπ. E2EE)** | Όχι | Το ZeroClaw συγχρονίζεται μέσω του API του Matrix. Δεν απαιτείται εισερχόμενο webhook. |
| **Discord/Slack** | Όχι | Το ίδιο — μόνο εξερχόμενες συνδέσεις. |
| **Nostr** | Όχι | Συνδέεται με relays μέσω WebSocket. Μόνο εξερχόμενες συνδέσεις. |
| **Gateway webhook** | Ναι | Τα POST /webhook, /whatsapp, /linq, /nextcloud-talk απαιτούν δημόσιο URL. |
| **Gateway pairing** | Ναι | Εάν αντιστοιχίζετε πελάτες μέσω της πύλης (gateway). |
| **Υπηρεσία Alpine/OpenRC** | Όχι | Υπηρεσία παρασκηνίου σε όλο το σύστημα στο Alpine Linux. |

**Σημείωση:** Τα Telegram, Discord, Slack και Nostr χρησιμοποιούν **εξερχόμενες συνδέσεις** — το ZeroClaw συνδέεται σε εξωτερικούς διακομιστές. Δεν απαιτείται προώθηση θυρών (port forwarding) ή δημόσια IP.

---

## 2. ZeroClaw σε Raspberry Pi

### 2.1 Προαπαιτούμενα

- Raspberry Pi (3/4/5) με Raspberry Pi OS.
- Περιφερειακά USB (Arduino, Nucleo) εάν χρησιμοποιείτε σειριακή μεταφορά.
- Προαιρετικά: `rppal` για εγγενές GPIO (λειτουργία `peripheral-rpi`).

### 2.2 Εγκατάσταση

```bash
# Μεταγλώττιση για RPi (ή διασταυρούμενη μεταγλώττιση από τον κεντρικό υπολογιστή)
cargo build --release --features hardware

# Ή εγκαταστήστε το μέσω της μεθόδου που προτιμάτε
```

### 2.3 Ρύθμιση

Επεξεργαστείτε το αρχείο `~/.zeroclaw/config.toml`:

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "rpi-gpio"
transport = "native"

# Ή Arduino μέσω USB
[[peripherals.boards]]
board = "arduino-uno"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200

[channels_config.telegram]
bot_token = "ΤΟ_TOKEN_ΤΟΥ_BOT_ΣΑΣ"
allowed_users = []

[gateway]
host = "127.0.0.1"
port = 42617
allow_public_bind = false
```

### 2.4 Εκτέλεση Δαίμονα (Μόνο τοπικά)

```bash
zeroclaw daemon --host 127.0.0.1 --port 42617
```

- Η πύλη (gateway) συνδέεται στο `127.0.0.1` — δεν είναι προσβάσιμη από άλλα μηχανήματα.
- Το κανάλι Telegram λειτουργεί: το ZeroClaw αντλεί δεδομένα από το API του Telegram (εξερχόμενη σύνδεση).
- Δεν απαιτείται τείχος προστασίας (firewall) ή προώθηση θυρών.

---

## 3. Σύνδεση στο 0.0.0.0 (Τοπικό Δίκτυο)

Για να επιτρέψετε σε άλλες συσκευές στο τοπικό σας δίκτυο (LAN) να έχουν πρόσβαση στην πύλη (π.χ. για αντιστοίχιση ή webhooks):

### 3.1 Επιλογή Α: Ρητή Ενεργοποίηση

```toml
[gateway]
host = "0.0.0.0"
port = 42617
allow_public_bind = true
```

```bash
zeroclaw daemon --host 0.0.0.0 --port 42617
```

**Ασφάλεια:** Η ρύθμιση `allow_public_bind = true` εκθέτει την πύλη στο τοπικό σας δίκτυο. Χρησιμοποιήστε την μόνο σε έμπιστα δίκτυα LAN.

### 3.2 Επιλογή Β: Σήραγγα (Tunnel - Προτείνεται για Webhooks)

Εάν χρειάζεστε ένα **δημόσιο URL** (π.χ. για WhatsApp webhook, εξωτερικούς πελάτες):

1. Εκτελέστε την πύλη στο localhost:
   ```bash
   zeroclaw daemon --host 127.0.0.1 --port 42617
   ```

2. Ξεκινήστε μια σήραγγα (tunnel):
   ```toml
   [tunnel]
   provider = "tailscale"   # ή "ngrok", "cloudflare"
   ```
   Ή χρησιμοποιήστε την εντολή `zeroclaw tunnel`.

3. Το ZeroClaw θα απορρίψει το `0.0.0.0` εκτός εάν η επιλογή `allow_public_bind = true` ή μια σήραγγα είναι ενεργή.

---

## 4. Telegram Polling (Χωρίς εισερχόμενη θύρα)

Το Telegram χρησιμοποιεί **long-polling** από προεπιλογή:

- Το ZeroClaw καλεί το `https://api.telegram.org/bot{token}/getUpdates`.
- Δεν απαιτείται εισερχόμενη θύρα ή δημόσια IP.
- Λειτουργεί πίσω από NAT, σε RPi, ή σε οικιακό lab.

**Ρύθμιση:**

```toml
[channels_config.telegram]
bot_token = "ΤΟ_TOKEN_ΤΟΥ_BOT_ΣΑΣ"
allowed_users = []            # Άρνηση από προεπιλογή, αντιστοιχίστε τις ταυτότητες ρητά
```

Εκτελέστε το `zeroclaw daemon` — το κανάλι Telegram ξεκινά αυτόματα.

Για την έγκριση ενός λογαριασμού Telegram κατά την εκτέλεση:

```bash
zeroclaw channel bind-telegram <ΤΑΥΤΟΤΗΤΑ>
```

Η `<ΤΑΥΤΟΤΗΤΑ>` μπορεί να είναι ένα αριθμητικό ID χρήστη Telegram ή ένα όνομα χρήστη (χωρίς το `@`).

### 4.1 Κανόνας Ενιαίου Poller (Σημαντικό)

Το API των Bot του Telegram υποστηρίζει μόνο έναν ενεργό poller ανά token.

- Διατηρήστε μόνο μία ενεργή εκτέλεση για το ίδιο token (συνιστάται: η υπηρεσία `zeroclaw daemon`).
- Μην εκτελείτε ταυτόχρονα το `cargo run -- channel start` ή άλλη διαδικασία bot.

Εάν δείτε το σφάλμα:
`Conflict: terminated by other getUpdates request`
σημαίνει ότι υπάρχει διένεξη. Σταματήστε τις επιπλέον εκτελέσεις και επανεκκινήστε μόνο έναν δαίμονα.

---

## 5. Κανάλια Webhook (WhatsApp, Nextcloud Talk, Προσαρμοσμένα)

Τα κανάλια που βασίζονται σε webhook χρειάζονται ένα **δημόσιο URL**, ώστε η Meta (WhatsApp) ή ο πελάτης σας να μπορούν να στέλνουν συμβάντα μέσω POST.

### 5.1 Tailscale Funnel

```toml
[tunnel]
provider = "tailscale"
```

Το Tailscale Funnel εκθέτει την πύλη σας μέσω ενός URL της μορφής `*.ts.net`. Δεν απαιτείται προώθηση θυρών.

### 5.2 ngrok

```toml
[tunnel]
provider = "ngrok"
```

Ή εκτελέστε το ngrok χειροκίνητα:
```bash
ngrok http 42617
# Χρησιμοποιήστε το HTTPS URL για το webhook σας
```

---

## 6. Λίστα Ελέγχου: Ανάπτυξη σε RPi

- [ ] Μεταγλώττιση με `--features hardware` (και `peripheral-rpi` για εγγενές GPIO).
- [ ] Ρύθμιση των ενοτήτων `[peripherals]` και `[channels_config.telegram]`.
- [ ] Εκτέλεση `zeroclaw daemon --host 127.0.0.1 --port 42617`.
- [ ] Για πρόσβαση σε LAN: `--host 0.0.0.0` + `allow_public_bind = true`.
- [ ] Για webhooks: χρήση Tailscale, ngrok ή Cloudflare tunnel.

---

## 7. OpenRC (Υπηρεσία Alpine Linux)

Το ZeroClaw υποστηρίζει το OpenRC για το Alpine Linux και άλλες διανομές που χρησιμοποιούν το σύστημα αρχικοποίησης OpenRC. Οι υπηρεσίες OpenRC εκτελούνται **σε όλο το σύστημα** και απαιτούν δικαιώματα root/sudo.

### 7.1 Προαπαιτούμενα

- Alpine Linux (ή άλλη διανομή βασισμένη στο OpenRC).
- Πρόσβαση Root ή sudo.
- Ένας αποκλειστικός χρήστης συστήματος `zeroclaw` (δημιουργείται κατά την εγκατάσταση).

### 7.2 Εγκατάσταση Υπηρεσίας

```bash
# Εγκατάσταση υπηρεσίας (το OpenRC εντοπίζεται αυτόματα στο Alpine)
sudo zeroclaw service install
```

Αυτό δημιουργεί:
- Σενάριο αρχικοποίησης (Init script): `/etc/init.d/zeroclaw`
- Κατάλογο ρυθμίσεων: `/etc/zeroclaw/`
- Κατάλογο καταγραφών (Logs): `/var/log/zeroclaw/`

### 7.3 Ρύθμιση

Συνήθως δεν απαιτείται χειροκίνητη αντιγραφή των ρυθμίσεων. Η εντολή `sudo zeroclaw service install` προετοιμάζει αυτόματα το `/etc/zeroclaw`, μεταφέρει την υπάρχουσα κατάσταση από τις ρυθμίσεις του χρήστη σας και ορίζει τις σωστές άδειες για τον χρήστη της υπηρεσίας `zeroclaw`.

### 7.4 Ενεργοποίηση και Έναρξη

```bash
# Προσθήκη στο προεπιλεγμένο επίπεδο εκτέλεσης (runlevel)
sudo rc-update add zeroclaw default

# Έναρξη της υπηρεσίας
sudo rc-service zeroclaw start

# Έλεγχος κατάστασης
sudo rc-service zeroclaw status
```

### 7.5 Διαχείριση Υπηρεσίας

| Εντολή | Περιγραφή |
|---------|-------------|
| `sudo rc-service zeroclaw start` | Έναρξη του δαίμονα |
| `sudo rc-service zeroclaw stop` | Διακοπή του δαίμονα |
| `sudo rc-service zeroclaw status` | Έλεγχος κατάστασης υπηρεσίας |
| `sudo rc-service zeroclaw restart` | Επανεκκίνηση του δαίμονα |

### 7.6 Καταγραφές (Logs)

Το OpenRC δρομολογεί τις καταγραφές στις εξής διαδρομές:

| Καταγραφή | Διαδρομή |
|-----|------|
| Πρόσβαση/stdout | `/var/log/zeroclaw/access.log` |
| Σφάλματα/stderr | `/var/log/zeroclaw/error.log` |

Προβολή καταγραφών:

```bash
sudo tail -f /var/log/zeroclaw/error.log
```

---

## 8. Αναφορές

- [channels-reference.md](./channels-reference.md) — Επισκόπηση ρυθμίσεων καναλιών
- [matrix-e2ee-guide.md](./matrix-e2ee-guide.md) — Ρύθμιση Matrix και επίλυση προβλημάτων E2EE
- [hardware-peripherals-design.md](./hardware-peripherals-design.md) — Σχεδιασμός περιφερειακών
