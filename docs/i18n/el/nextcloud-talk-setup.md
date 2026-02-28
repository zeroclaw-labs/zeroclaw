# Ρύθμιση Nextcloud Talk

Αυτός ο οδηγός περιγράφει τη διαδικασία ενσωμάτωσης του Nextcloud Talk με το ZeroClaw.

## 1. Λειτουργία Ενσωμάτωσης

- Λήψη συμβάντων webhook από bot του Talk μέσω της διαδρομής `POST /nextcloud-talk`.
- Επαλήθευση ακεραιότητας μηνυμάτων (HMAC-SHA256) μέσω κοινού μυστικού (shared secret).
- Αποστολή απαντήσεων στα δωμάτια του Talk μέσω του Nextcloud OCS API.

## 2. Ρύθμιση

Στο αρχείο `~/.zeroclaw/config.toml`, προσθέστε την ενότητα `[channels_config.nextcloud_talk]`:

```toml
[channels_config.nextcloud_talk]
base_url = "https://cloud.example.com"
app_token = "YOUR_APP_TOKEN"
webhook_secret = "YOUR_WEBHOOK_SECRET" # Προαιρετικό
allowed_users = ["*"]
```

### Παράμετροι Ρύθμισης

- `base_url`: Το βασικό URL της εγκατάστασης Nextcloud.
- `app_token`: Το διακριτικό πρόσβασης (app token) του bot για την εξουσιοδότηση στο OCS API.
- `webhook_secret`: Το κοινό μυστικό για την επαλήθευση της κεφαλίδας `X-Nextcloud-Talk-Signature`.
- `allowed_users`: Λίστα με επιτρεπόμενα ID χρηστών (actors). Χρησιμοποιήστε `["*"]` για καθολική πρόσβαση.

> **Συμβουλή**: Μπορείτε να χρησιμοποιήσετε τη μεταβλητή περιβάλλοντος `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET` για να παρακάμψετε τη ρύθμιση του αρχείου.

## 3. Ρύθμιση Τελικού Σημείου (Endpoint)

Ξεκινήστε τον daemon του ZeroClaw για να εκθέσετε το webhook:

```bash
zeroclaw daemon
```

Στο Nextcloud Talk, ορίστε το URL του webhook για το bot σας ως:
`https://<YOUR_PUBLIC_URL>/nextcloud-talk`

## 4. Επαλήθευση Υπογραφής

Εάν έχει οριστεί `webhook_secret`, το ZeroClaw επαληθεύει τις κεφαλίδες:
- `X-Nextcloud-Talk-Random`
- `X-Nextcloud-Talk-Signature`

Ο αλγόριθμος επαλήθευσης είναι: `hex(hmac_sha256(secret, random + raw_request_body))`. Σε περίπτωση αποτυχίας, η πύλη επιστρέφει σφάλμα `401 Unauthorized`.

## 5. Φιλτράρισμα και Δρομολόγηση

- Το ZeroClaw αγνοεί συμβάντα που προέρχονται από άλλα bots (`actorType = bots`).
- Αγνοούνται συμβάντα συστήματος ή συμβάντα που δεν περιέχουν μηνύματα.
- Οι απαντήσεις δρομολογούνται αυτόματα στο σωστό δωμάτιο χρησιμοποιώντας το token δωματίου από το payload του webhook.

## 6. Βήματα Επαλήθευσης

1. Ορίστε προσωρινά `allowed_users = ["*"]`.
2. Στείλτε ένα δοκιμαστικό μήνυμα στο δωμάτιο του Talk.
3. Επιβεβαιώστε τη λήψη και την απάντηση από το ZeroClaw.
4. Περιορίστε την πρόσβαση ορίζοντας συγκεκριμένα ID χρηστών στο `allowed_users`.

## 7. Αντιμετώπιση Προβλημάτων

- **404 Not Configured**: Βεβαιωθείτε ότι υπάρχει η ενότητα `[channels_config.nextcloud_talk]`.
- **401 Invalid Signature**: Ελέγξτε εάν το `webhook_secret` ταυτίζεται με αυτό που έχει οριστεί στο Nextcloud.
- **200 OK χωρίς απάντηση**: Το μήνυμα πιθανώς φιλτραρίστηκε (π.χ. προέρχεται από bot ή μη εξουσιοδοτημένο χρήστη).
