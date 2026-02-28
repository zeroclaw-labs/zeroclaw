# Λειτουργία και Υλοποίηση (Operations & Deployment)

Τεχνική τεκμηρίωση για τον χειρισμό, τη συντήρηση και την ανάπτυξη του ZeroClaw σε περιβάλλοντα παραγωγής.

---

## 1. Βασικές Λειτουργίες και Εγχειρίδια

- **Operations Runbook (Day-2)**: [../operations-runbook.md](../operations-runbook.md)
- **Runbook Probes Συνδεσιμότητας Παρόχων στο CI**: [connectivity-probes-runbook.md](connectivity-probes-runbook.md)
- **Διαδικασία Έκδοσης (Release Process)**: [../release-process.md](../release-process.md)
- **Αντιμετώπιση Προβλημάτων (Troubleshooting)**: [../troubleshooting.md](../troubleshooting.md)
- **Ανάπτυξη Δικτύου και Πύλης (Gateway)**: [../network-deployment.md](../network-deployment.md)
- **Ρύθμιση Mattermost**: [../mattermost-setup.md](../mattermost-setup.md)

---

## 2. Τυπική Ροή Εργασιών Συντήρησης

1. **Επαλήθευση Περιβάλλοντος**: Χρήση των εντολών `status`, `doctor` και `channel doctor`.
2. **Διαχείριση Ρυθμίσεων**: Εφαρμογή μεμονωμένων αλλαγών στο αρχείο παραμέτρων (Config).
3. **Επανεκκίνηση Υπηρεσιών**: Ανανέωση των daemons για την εφαρμογή των αλλαγών.
4. **Έλεγχος Υγείας (Health Check)**: Επιβεβαίωση της σωστής λειτουργίας καναλιών και πύλης.
5. **Επαναφορά (Rollback)**: Άμεση επιστροφή σε προηγούμενη σταθερή κατάσταση σε περίπτωση δυσλειτουργίας.

---

## 3. Σχετικά Έγγραφα

- **Αναφορά Παραμέτρων (Config Reference)**: [../config-reference.md](../config-reference.md)
- **Πολιτικές Ασφάλειας (Security)**: [../security/README.md](../security/README.md)
