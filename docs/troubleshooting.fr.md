# Dépannage ZeroClaw

Ce guide couvre les pannes les plus fréquentes (installation et runtime) et des chemins de résolution rapides.

Dernière vérification : **21 février 2026**.

## Installation / Bootstrap

### `cargo` introuvable

Symptôme :

- le bootstrap s'arrête avec `cargo is not installed`

Correctif :

```bash
./bootstrap.sh --install-rust
```

Ou installez Rust depuis <https://rustup.rs/>.

### Dépendances système de build manquantes

Symptôme :

- le build échoue à cause du compilateur ou de `pkg-config`

Correctif :

```bash
./bootstrap.sh --install-system-deps
```

### Commande `zeroclaw` introuvable après installation

Symptôme :

- installation réussie mais le shell ne trouve pas `zeroclaw`

Correctif :

```bash
export PATH="$HOME/.cargo/bin:$PATH"
which zeroclaw
```

Ajoutez la ligne au profil shell si nécessaire.

## Runtime / Gateway

### Gateway injoignable

Vérifications :

```bash
zeroclaw status
zeroclaw doctor
```

Puis vérifiez `~/.zeroclaw/config.toml` :

- `[gateway].host` (défaut `127.0.0.1`)
- `[gateway].port` (défaut `3000`)
- activer `allow_public_bind` seulement si l'exposition LAN/public est volontaire

### Erreurs pairing / auth sur webhook

Vérifications :

1. confirmer la fin du pairing (flux `/pair`)
2. vérifier que le bearer token est à jour
3. relancer les diagnostics :

```bash
zeroclaw doctor
```

## Problèmes de channels

### Conflit Telegram : `terminated by other getUpdates request`

Cause :

- plusieurs pollers utilisent le même token bot

Correctif :

- conserver un seul runtime actif pour ce token
- arrêter les processus `zeroclaw daemon` / `zeroclaw channel start` en trop

### Channel en état unhealthy dans `channel doctor`

Vérifications :

```bash
zeroclaw channel doctor
```

Puis valider les identifiants du channel et les champs d'allowlist dans la config.

## Mode service

### Service installé mais non démarré

Vérifications :

```bash
zeroclaw service status
```

Récupération :

```bash
zeroclaw service stop
zeroclaw service start
```

Logs Linux :

```bash
journalctl --user -u zeroclaw.service -f
```

## Compatibilité installateur historique

Les deux commandes restent valides :

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/bootstrap.sh | bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/install.sh | bash
```

`install.sh` est un point d'entrée de compatibilité et relaie vers le comportement bootstrap si nécessaire.

## Toujours bloqué ?

Incluez ces sorties lors d'une ouverture d'issue :

```bash
zeroclaw --version
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
```

Ajoutez aussi l'OS, la méthode d'installation, et des extraits de config anonymisés (sans secrets).

## Docs liées

- [operations-runbook.md](operations-runbook.md)
- [one-click-bootstrap.md](one-click-bootstrap.md)
- [channels-reference.md](channels-reference.md)
- [network-deployment.md](network-deployment.md)
