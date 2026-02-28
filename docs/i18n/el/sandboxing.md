# Στρατηγικές Sandboxing για το ZeroClaw

> [!WARNING]
> **Κατάσταση: Πρόταση / Οδικός Χάρτης**
>
> Αυτό το έγγραφο περιγράφει προτεινόμενες προσεγγίσεις περιορισμού. Για την τρέχουσα υλοποίηση, ανατρέξτε στα [config-reference.md](config-reference.md) και [operations-runbook.md](operations-runbook.md).

## Περιγραφή Προβλήματος

Το ZeroClaw εφαρμόζει ασφάλεια σε επίπεδο εφαρμογής (allowlists, path validation, injection prevention), αλλά δεν διαθέτει απομόνωση σε επίπεδο λειτουργικού συστήματος. Χωρίς sandboxing, ένας εξουσιοδοτημένος χρήστης μπορεί να εκτελέσει εντολές με τα πλήρη δικαιώματα της διεργασίας του ZeroClaw.

---

## Προτεινόμενες Τεχνικές Προσεγγίσεις

### 1. Firejail (Προτεινόμενο για Linux)

Το Firejail παρέχει απομόνωση σε επίπεδο χρήστη (User Space) με ελάχιστη επιβάρυνση πόρων.

```rust
// Παράδειγμα υλοποίησης περιβλήματος (Wrapper)
impl FirejailSandbox {
    pub fn wrap_command(&self, cmd: &mut Command) -> &mut Command {
        if !self.enabled { return cmd; }

        let mut jail = Command::new("firejail");
        jail.args([
            "--private=home",    // Απομόνωση προσωπικού καταλόγου
            "--private-dev",     // Περιορισμένη πρόσβαση σε συσκευές
            "--nosound",         // Απενεργοποίηση ήχου
            "--no3d",            // Απενεργοποίηση επιτάχυνσης γραφικών
            "--quiet",           // Μείωση θορύβου καταγραφής
        ]);

        // Ενσωμάτωση της αρχικής εντολής στο sandbox
        // ...
    }
}
```

### 2. Bubblewrap (Unprivileged Sandboxing)

Χρήση kernel namespaces για τη δημιουργία εφήμερων περιβαλλόντων χωρίς την ανάγκη δικαιωμάτων root.

```bash
# Παράδειγμα περιορισμού πρόσβασης με bwrap
bwrap --ro-bind /usr /usr \
      --proc /proc \
      --dev /dev \
      --unshare-all \
      --share-net \
      --die-with-parent \
      -- /bin/sh -c "command"
```

### 3. Landlock (Native Linux LSM)

Περιορισμός πρόσβασης στο σύστημα αρχείων μέσω του εγγενούς μηχανισμού του πυρήνα Linux, χωρίς τη χρήση εξωτερικών εργαλείων.

---

## Μήτρα Προτεραιοποίησης και Ασφάλειας

| Φάση | Λύση | Προσπάθεια | Επίπεδο Απομόνωσης |
|:---:|:---|:---:|:---:|
| **P0** | Landlock (Native Linux) | Χαμηλή | Σύστημα Αρχείων |
| **P1** | Ενσωμάτωση Firejail | Χαμηλή | Πλήρες User Space |
| **P2** | Bubblewrap Wrapper | Μέτρια | Kernel Namespaces |
| **P3** | Ephemeral Docker Sandbox | Υψηλή | Πλήρης Εικονικοποίηση |

---

## Προτεινόμενη Διαμόρφωση (Config Schema)

```toml
[security.sandbox]
enabled = true
backend = "auto"  # Επιλογές: auto, firejail, bubblewrap, landlock, docker, none

# Ρυθμίσεις Firejail
[security.sandbox.firejail]
extra_args = ["--seccomp", "--caps.drop=all"]

# Ρυθμίσεις Landlock
[security.sandbox.landlock]
readonly_paths = ["/usr", "/bin", "/lib"]
readwrite_paths = ["$HOME/workspace", "/tmp/zeroclaw"]
```

---

## Στρατηγική Επαλήθευσης

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn sandbox_blocks_unauthorized_access() {
        // Επαλήθευση αποκλεισμού πρόσβασης σε ευαίσθητα αρχεία (π.χ. /etc/shadow)
        let result = sandboxed_execute("cat /etc/shadow");
        assert!(result.is_err());
    }
}
```
