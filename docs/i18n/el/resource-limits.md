# Όρια Πόρων (Resource Limits)

> [!WARNING]
> **Κατάσταση: Πρόταση / Οδικός Χάρτης**
>
> Αυτό το έγγραφο περιγράφει προτεινόμενες προσεγγίσεις περιορισμού πόρων. Για την τρέχουσα υλοποίηση, ανατρέξτε στα έγγραφα [config-reference.md](config-reference.md) και [operations-runbook.md](operations-runbook.md).

## Περιγραφή Προβλήματος

Το ZeroClaw εφαρμόζει περιορισμό ρυθμού (Rate Limiting - 20 ενέργειες/ώρα), αλλά δεν διαθέτει ανώτατα όρια χρήσης πόρων συστήματος. Χωρίς αυτούς τους περιορισμούς, ένας πράκτορας ενδέχεται να:
- Καταναλώσει υπερβολική μνήμη (RAM).
- Προκαλέσει υψηλό φόρτο CPU (100%).
- Εξαντλήσει τον αποθηκευτικό χώρο (disk space) με logs ή προσωρινά αρχεία.

---

## Προτεινόμενες Τεχνικές Προσεγγίσεις

### 1. cgroups v2 (Linux)

Αυτή είναι η συνιστώμενη μέθοδος για την απομόνωση των πόρων του ZeroClaw σε επίπεδο λειτουργικού συστήματος.

```bash
# Παράδειγμα υπηρεσίας systemd με περιορισμούς
[Service]
MemoryMax=512M
CPUQuota=100%
IOReadBandwidthMax=/dev/sda 10M
IOWriteBandwidthMax=/dev/sda 10M
TasksMax=100
```

### 2. Έλεγχος Ασύγχρονων Εργασιών (Tokio Tasks)

Ενσωμάτωση χρονικών ορίων (timeouts) για την αποφυγή κατάληψης των νημάτων (task starvation).

```rust
use tokio::time::{timeout, Duration};

pub async fn execute_with_timeout<F, T>(
    fut: F,
    cpu_time_limit: Duration,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    // Περιορισμός χρόνου εκτέλεσης
    timeout(cpu_time_limit, fut).await?
}
```

### 3. Διαχείριση Μνήμης (Memory Monitoring)

Εποπτεία της χρήσης του σωρού (heap) και αυτόματος τερματισμός σε περίπτωση υπέρβασης των ορίων.

```rust
use std::alloc::{GlobalAlloc, Layout, System};

struct LimitedAllocator<A> {
    inner: A,
    max_bytes: usize,
    used: std::sync::atomic::AtomicUsize,
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for LimitedAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let current = self.used.fetch_add(size, std::sync::atomic::Ordering::Relaxed);
        if current + size > self.max_bytes {
            std::process::abort();
        }
        self.inner.alloc(layout)
    }
}
```

---

## Προτεινόμενη Διαμόρφωση (Config Schema)

```toml
[resources]
# Όρια μνήμης (MB)
max_memory_mb = 512
max_memory_per_command_mb = 128

# Όρια CPU
max_cpu_percent = 50
max_cpu_time_seconds = 60

# Όρια αποθήκευσης (I/O)
max_log_size_mb = 100
max_temp_storage_mb = 500

# Όρια διεργασιών και αρχείων
max_subprocesses = 10
max_open_files = 100
```

---

## Σχέδιο Υλοποίησης ανά Προτεραιότητα

| Προτεραιότητα | Χαρακτηριστικό | Δυσκολία | Αντίκτυπος |
|:---:|:---|:---:|:---:|
| **P0** | Παρακολούθηση μνήμης & Fail-safe τερματισμός | Χαμηλή | Υψηλός |
| **P1** | Timeouts CPU ανά εντολή | Χαμηλή | Υψηλός |
| **P2** | Υποστήριξη cgroups (Linux) | Μέτρια | Πολύ Υψηλός |
| **P3** | Περιορισμοί I/O δίσκου | Μέτρια | Μέτριος |
