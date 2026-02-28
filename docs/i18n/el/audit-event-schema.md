# Σχήμα Audit Event CI/Security (Ελληνικά)

Αυτή η σελίδα είναι συνοπτική τοπικοποιημένη γέφυρα για το σχήμα συμβάντων audit.

Αγγλικό πρωτότυπο:

- [../../audit-event-schema.md](../../audit-event-schema.md)

## Βασικά σημεία

- Κανονικό envelope: `zeroclaw.audit.v1`.
- Κύρια πεδία: `event_type`, `generated_at`, `run_context`, `artifact`, `payload`.
- Πίνακας retention ανά workflow για artifacts/audit lanes.

Για πλήρη schema και πίνακες, χρησιμοποιήστε το αγγλικό κείμενο.
