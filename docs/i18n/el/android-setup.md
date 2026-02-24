# Εγκατάσταση σε Android

Το ZeroClaw παρέχει προκατασκευασμένα εκτελέσιμα αρχεία (binaries) για συσκευές Android.

## Υποστηριζόμενες Αρχιτεκτονικές

| Στόχος | Έκδοση Android | Συσκευές |
|--------|----------------|----------|
| `armv7-linux-androideabi` | Android 4.1+ (API 16+) | Παλιά 32-bit τηλέφωνα (Galaxy S3, κ.λπ.) |
| `aarch64-linux-android` | Android 5.0+ (API 21+) | Σύγχρονα 64-bit τηλέφωνα |

## Εγκατάσταση μέσω Termux

Ο ευκολότερος τρόπος εκτέλεσης του ZeroClaw σε Android είναι μέσω [Termux](https://termux.dev/).

### 1. Εγκατάσταση Termux

Κατεβάστε από το [F-Droid](https://f-droid.org/packages/com.termux/) (προτείνεται) ή από τις εκδόσεις GitHub.

> ⚠️ **Σημείωση:** Η έκδοση του Play Store είναι παρωχημένη και δεν υποστηρίζεται.

### 2. Λήψη ZeroClaw

```bash
# Έλεγχος αρχιτεκτονικής
uname -m
# aarch64 = 64-bit, armv7l/armv8l = 32-bit

# Λήψη του κατάλληλου binary
# Για 64-bit (aarch64):
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-linux-android.tar.gz
tar xzf zeroclaw-aarch64-linux-android.tar.gz

# Για 32-bit (armv7):
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-armv7-linux-androideabi.tar.gz
tar xzf zeroclaw-armv7-linux-androideabi.tar.gz
```

### 3. Εγκατάσταση και Εκτέλεση

```bash
chmod +x zeroclaw
mv zeroclaw $PREFIX/bin/

# Επαλήθευση εγκατάστασης
zeroclaw --version

# Εκτέλεση ρύθμισης
zeroclaw onboard
```

## Άμεση Εγκατάσταση μέσω ADB

Για προχωρημένους χρήστες που θέλουν να εκτελέσουν το ZeroClaw εκτός Termux:

```bash
# Από τον υπολογιστή σας με ADB
adb push zeroclaw /data/local/tmp/
adb shell chmod +x /data/local/tmp/zeroclaw
adb shell /data/local/tmp/zeroclaw --version
```

> ⚠️ Η εκτέλεση εκτός Termux απαιτεί συσκευή με root ή συγκεκριμένα δικαιώματα για πλήρη λειτουργικότητα.

## Περιορισμοί στο Android

- **Χωρίς systemd:** Χρησιμοποιήστε το `termux-services` του Termux για λειτουργία daemon
- **Πρόσβαση αρχείων:** Απαιτεί δικαιώματα αποθήκευσης Termux (`termux-setup-storage`)
- **Δίκτυο:** Ορισμένες λειτουργίες ενδέχεται να απαιτούν δικαίωμα Android VPN για τοπική δέσμευση

## Κατασκευή από Πηγαίο Κώδικα

Για να κατασκευάσετε για Android μόνοι σας:

```bash
# Εγκατάσταση Android NDK
# Προσθήκη στόχων
rustup target add armv7-linux-androideabi aarch64-linux-android

# Ορισμός μονοπατιού NDK
export ANDROID_NDK_HOME=/path/to/ndk
export PATH=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin:$PATH

# Κατασκευή
cargo build --release --target armv7-linux-androideabi
cargo build --release --target aarch64-linux-android
```

## Αντιμετώπιση Προβλημάτων

### "Permission denied"

```bash
chmod +x zeroclaw
```

### "not found" ή σφάλματα linker

Βεβαιωθείτε ότι κατεβάσατε το σωστό binary για την αρχιτεκτονική της συσκευής σας.

### Παλιό Android (4.x)

Χρησιμοποιήστε το build `armv7-linux-androideabi` με API level 16+.
