# Καταγραφή Ελέγχου (Audit Logging)

> **Κατάσταση**: Πρόταση / Οδικός Χάρτης (Roadmap)
>
> Αυτό το έγγραφο περιγράφει προτεινόμενες προσεγγίσεις και ενδέχεται να περιλαμβάνει υποθετικές εντολές ή ρυθμίσεις. Για την τρέχουσα συμπεριφορά, ανατρέξτε στα έγγραφα: [config-reference.md](config-reference.md), [operations-runbook.md](operations-runbook.md) και [troubleshooting.md](troubleshooting.md).

## Περιγραφή Προβλήματος

Το ZeroClaw απαιτεί έναν μηχανισμό καταγραφής ελέγχου (audit trails) με προστασία από παραποίηση, προκειμένου να τεκμηριώνονται:
- Η ταυτότητα του χρήστη που εκτέλεσε μια εντολή.
- Η χρονική στιγμή και το κανάλι επικοινωνίας.
- Οι πόροι που προσπελάστηκαν.
- Η εφαρμογή των πολιτικών ασφαλείας.

---

## Προτεινόμενη Μορφή Συμβάντος (Log Format)

```json
{
  "timestamp": "2026-02-16T12:34:56Z",
  "event_id": "evt_1a2b3c4d",
  "event_type": "command_execution",
  "actor": {
    "channel": "telegram",
    "user_id": "123456789",
    "username": "@alice"
  },
  "action": {
    "command": "ls -la",
    "risk_level": "low",
    "approved": false,
    "allowed": true
  },
  "result": {
    "success": true,
    "exit_code": 0,
    "duration_ms": 15
  },
  "security": {
    "policy_violation": false,
    "rate_limit_remaining": 19
  },
  "signature": "SHA256:abc123..."  // Υπογραφή HMAC για ακεραιότητα δεδομένων
}
```

---

## Υλοποίηση (Implementation)

```rust
// src/security/audit.rs

pub enum AuditEventType {
    CommandExecution,
    FileAccess,
    ConfigurationChange,
    AuthSuccess,
    AuthFailure,
    PolicyViolation,
}

pub struct AuditLogger {
    log_path: PathBuf,
    signing_key: Option<hmac::Hmac<sha2::Sha256>>,
}

impl AuditLogger {
    pub fn log(&self, event: &AuditEvent) -> anyhow::Result<()> {
        let mut line = serde_json::to_string(event)?;

        // Προσθήκη υπογραφής HMAC για προστασία από παραποίηση
        if let Some(ref key) = self.signing_key {
            let signature = compute_hmac(key, line.as_bytes());
            line.push_str(&format!(",\"signature\": \"{}\"", signature));
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        writeln!(file, "{}", line)?;
        file.sync_all()?; // Διασφάλιση εγγραφής στον δίσκο
        Ok(())
    }
}
```

---

## Σχήμα Ρυθμίσεων (Config Schema)

```toml
[security.audit]
enabled = true
log_path = "~/.config/zeroclaw/audit.log"
max_size_mb = 100
rotate = "daily"  # Επιλογές: daily | weekly | size

# Προστασία ακεραιότητας (Tamper evidence)
sign_events = true
signing_key_path = "~/.config/zeroclaw/audit.key"

# Πεδίο εφαρμογής καταγραφής
log_commands = true
log_file_access = true
log_auth_events = true
log_policy_violations = true
```

---

## CLI Διαχείρισης Ελέγχου

```bash
# Αναζήτηση εντολών από συγκεκριμένο χρήστη
zeroclaw audit --user @alice

# Προβολή συμβάντων υψηλού κινδύνου
zeroclaw audit --risk high

# Εμφάνιση παραβιάσεων πολιτικής του τελευταίου 24ώρου
zeroclaw audit --since 24h --violations-only

# Επαλήθευση ακεραιότητας των αρχείων καταγραφής
zeroclaw audit --verify-signatures
```

---

## Διαχείριση Αρχείων (Log Rotation)

Το σύστημα υποστηρίζει αυτόματη εναλλαγή αρχείων όταν συμπληρωθεί το μέγιστο μέγεθος ή παρέλθει το ορισμένο χρονικό διάστημα.

```rust
pub fn rotate_audit_log(log_path: &PathBuf, max_size: u64) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(log_path)?;
    if metadata.len() < max_size {
        return Ok(());
    }
    // Διαδικασία εναλλαγής αρχείων (π.χ. audit.log -> audit.log.1)
    Ok(())
}
```

---

## Προτεραιότητες Υλοποίησης

| Φάση | Λειτουργικότητα | Επίπεδο Προσπάθειας | Αξία Ασφαλείας |
|:---|:---|:---|:---|
| **P0** | Βασική καταγραφή συμβάντων | Χαμηλό | Μέτρια |
| **P1** | CLI αναζήτησης και αναφορών | Μέτριο | Μέτρια |
| **P2** | Ψηφιακή υπογραφή HMAC | Μέτριο | Υψηλή |
| **P3** | Αυτόματη εναλλαγή και αρχειοθέτηση | Χαμηλό | Μέτρια |
