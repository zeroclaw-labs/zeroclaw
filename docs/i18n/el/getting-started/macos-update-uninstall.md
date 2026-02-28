# Οδηγός Ενημέρωσης και Απεγκατάστασης στο macOS

Αυτή η σελίδα τεκμηριώνει τις υποστηριζόμενες διαδικασίες ενημέρωσης και απεγκατάστασης του ZeroClaw στο macOS (OS X).

Τελευταία επαλήθευση: **22 Φεβρουαρίου 2026**.

## 1) Έλεγχος τρέχουσας μεθόδου εγκατάστασης

```bash
which zeroclaw
zeroclaw --version
```

Τυπικές τοποθεσίες:

- Homebrew: `/opt/homebrew/bin/zeroclaw` (Apple Silicon) ή `/usr/local/bin/zeroclaw` (Intel)
- Cargo/bootstrap/χειροκίνητη: `~/.cargo/bin/zeroclaw`

Αν υπάρχουν και οι δύο, η σειρά `PATH` του shell σας καθορίζει ποια εκτελείται.

## 2) Ενημέρωση στο macOS

### Α) Εγκατάσταση μέσω Homebrew

```bash
brew update
brew upgrade zeroclaw
zeroclaw --version
```

### Β) Εγκατάσταση μέσω Clone + bootstrap

Από τον τοπικό κλώνο του αποθετηρίου:

```bash
git pull --ff-only
./bootstrap.sh --prefer-prebuilt
zeroclaw --version
```

Αν θέλετε ενημέρωση μόνο από πηγαίο κώδικα:

```bash
git pull --ff-only
cargo install --path . --force --locked
zeroclaw --version
```

### Γ) Χειροκίνητη εγκατάσταση προκατασκευασμένου binary

Επαναλάβετε τη ροή λήψης/εγκατάστασης με το πιο πρόσφατο αρχείο έκδοσης και επαληθεύστε:

```bash
zeroclaw --version
```

## 3) Απεγκατάσταση στο macOS

### Α) Διακοπή και αφαίρεση υπηρεσίας background πρώτα

Αυτό αποτρέπει τη συνέχεια εκτέλεσης του daemon μετά την αφαίρεση του binary.

```bash
zeroclaw service stop || true
zeroclaw service uninstall || true
```

Αντικείμενα υπηρεσίας που αφαιρούνται από την `service uninstall`:

- `~/Library/LaunchAgents/com.zeroclaw.daemon.plist`

### Β) Αφαίρεση binary ανά μέθοδο εγκατάστασης

Homebrew:

```bash
brew uninstall zeroclaw
```

Cargo/bootstrap/χειροκίνητη (`~/.cargo/bin/zeroclaw`):

```bash
cargo uninstall zeroclaw || true
rm -f ~/.cargo/bin/zeroclaw
```

### Γ) Προαιρετικά: αφαίρεση τοπικών δεδομένων εκτέλεσης

Εκτελέστε αυτό μόνο αν θέλετε πλήρη εκκαθάριση ρυθμίσεων, προφίλ auth, logs και κατάστασης workspace.

```bash
rm -rf ~/.zeroclaw
```

## 4) Επαλήθευση ολοκλήρωσης απεγκατάστασης

```bash
command -v zeroclaw || echo "zeroclaw binary not found"
pgrep -fl zeroclaw || echo "No running zeroclaw process"
```

Αν το `pgrep` εξακολουθεί να βρίσκει διεργασία, σταματήστε την χειροκίνητα και ελέγξτε ξανά:

```bash
pkill -f zeroclaw
```

## Σχετικά Έγγραφα

- [One-Click Bootstrap](../one-click-bootstrap.md)
- [Αναφορά Εντολών](../commands-reference.md)
- [Αντιμετώπιση Προβλημάτων](../troubleshooting.md)
