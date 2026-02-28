# Αγνωστικιστική Ασφάλεια: Μηδενικός Αντίκτυπος στη Φορητότητα

> **Κατάσταση**: Πρόταση / Οδικός Χάρτης (Roadmap)
>
> Αυτό το έγγραφο περιγράφει προτεινόμενες προσεγγίσεις και ενδέχεται να περιλαμβάνει υποθετικές εντολές ή ρυθμίσεις. Για την τρέχουσα λειτουργία, ανατρέξτε στα έγγραφα: [config-reference.md](config-reference.md), [operations-runbook.md](operations-runbook.md) και [troubleshooting.md](troubleshooting.md).

## Βασικά Ερωτήματα Σχεδιασμού

Θα προκαλέσουν οι λειτουργίες ασφαλείας προβλήματα στην:
1. Ταχύτητα των cross-compilation builds;
2. Αρθρωτή αρχιτεκτονική (δυνατότητα αντικατάστασης στοιχείων);
3. Υποστήριξη διαφορετικού υλικού (ARM, x86, RISC-V);
4. Υποστήριξη περιορισμένων πόρων (<5MB RAM);

**Απάντηση: Όχι** — Η ασφάλεια υλοποιείται μέσω προαιρετικών **feature flags** και **υποθετικής μεταγλώττισης (conditional compilation)** ανά πλατφόρμα.

---

## 1. Ταχύτητα Build: Διαχείριση μέσω Features

### Cargo.toml: Λειτουργίες Ασφαλείας

```toml
[features]
default = ["basic-security"]

# Βασική ασφάλεια (μόνιμα ενεργή, ελάχιστη επιβάρυνση)
basic-security = []

# Sandboxing (προαιρετική ενεργοποίηση ανά πλατφόρμα)
sandbox-landlock = []  # Linux 5.13+
sandbox-firejail = []  # Linux
sandbox-bubblewrap = []# macOS/Linux
sandbox-docker = []    # Υποστήριξη Docker (υψηλή επιβάρυνση)

# Πλήρης σουίτα ασφαλείας για περιβάλλοντα παραγωγής
security-full = [
    "basic-security",
    "sandbox-landlock",
    "resource-monitoring",
    "audit-logging",
]

# Παρακολούθηση πόρων και καταγραφή ελέγχου (Audit)
resource-monitoring = []
audit-logging = []
```

### Εντολές Μεταγλώττισης

```bash
# Γρήγορο dev build (χωρίς πρόσθετα ασφαλείας)
cargo build --profile dev

# Release build με βασική ασφάλεια (προεπιλογή)
cargo build --release

# Πλήρες build παραγωγής με όλες τις λειτουργίες ασφαλείας
cargo build --release --features security-full
```

### Υποθετική Μεταγλώττιση

Όταν οι λειτουργίες είναι απενεργοποιημένες, ο σχετικός κώδικας εξαιρείται πλήρως από τη μεταγλώττιση, διασφαλίζοντας ότι το μέγεθος του εκτελέσιμου (binary) παραμένει μικρό.

```rust
// src/security/mod.rs

#[cfg(feature = "sandbox-landlock")]
mod landlock;
#[cfg(feature = "sandbox-landlock")]
pub use landlock::LandlockSandbox;

// Η βασική ασφάλεια περιλαμβάνεται πάντα
pub mod policy; // Allowlist, path blocking, injection protection
```

---

## 2. Αρθρωτή Αρχιτεκτονική: Ασφάλεια ως Trait

Η ασφάλεια υλοποιείται ως εναλλάξιμο στοιχείο μέσω του trait `Sandbox`:

```rust
// src/security/traits.rs

#[async_trait]
pub trait Sandbox: Send + Sync {
    /// Εφαρμογή προστασίας sandbox σε μια εντολή
    fn wrap_command(&self, cmd: &mut std::process::Command) -> std::io::Result<()>;

    /// Έλεγχος διαθεσιμότητας του sandbox στην τρέχουσα πλατφόρμα
    fn is_available(&self) -> bool;

    /// Όνομα του μηχανισμού
    fn name(&self) -> &str;
}
```

---

## 3. Υποστήριξη Πολλαπλών Πλατφορμών

Το ZeroClaw προσαρμόζει αυτόματα το επίπεδο προστασίας βάσει των δυνατοτήτων του λειτουργικού συστήματος:

| Πλατφόρμα | Κατάσταση Build | Μηχανισμός Runtime |
|:---|:---|:---|
| Linux ARM (Raspberry Pi) | ✅ Επιτυχές | Landlock ή None |
| Linux x86_64 | ✅ Επιτυχές | Landlock ή Firejail |
| macOS ARM (M1/M2) | ✅ Επιτυχές | Bubblewrap ή None |
| Windows x86_64 | ✅ Επιτυχές | Επίπεδο Εφαρμογής |
| RISC-V Linux | ✅ Επιτυχές | Landlock ή None |

---

## 4. Περιορισμένοι Πόροι: Ανάλυση Επιβάρυνσης

| Λειτουργία | Μέγεθος στο Binary (περίπου) | Επιβάρυνση RAM |
|:---|:---|:---|
| Base ZeroClaw | 3.4MB | <5MB |
| + Landlock | +50KB | +100KB |
| + Παρακολούθηση Πόρων | +30KB | +50KB |
| **Σύνολο (Πλήρης Ασφάλεια)** | **+140KB** | **<6MB** |

---

## 5. Δυνατότητα Εναλλαγής (Agnostic Swaps)

Μπορείτε να αλλάξετε τον μηχανισμό ασφαλείας μέσω του αρχείου ρυθμίσεων:

```toml
# Χρήση Landlock (Native Linux LSM)
[security.sandbox]
backend = "landlock"

# Χρήση Docker (Μέγιστη απομόνωση)
[security.sandbox]
backend = "docker"
```

---

## Σύνοψη Αρχών Σχεδιασμού

| Κριτήριο | Πριν | Μετά (με Ασφάλεια) | Κατάσταση |
|:---|:---|:---|:---|
| Χρήση Μνήμης | <5MB RAM | <6MB RAM | ✅ Διατηρείται |
| Ταχύτητα Εκκίνησης | <10ms | <15ms | ✅ Διατηρείται |
| Συμβατότητα Υλικού | Πλήρης | Πλήρης | ✅ Διατηρείται |
| Αρθρωτή Σχεδίαση | Ναι | Ναι | ✅ Ενισχυμένη |

**Η ασφάλεια παραμένει προαιρετική, αποδοτική και συμβατή με κάθε πλατφόρμα.**
