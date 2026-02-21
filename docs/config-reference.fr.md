# Référence de configuration ZeroClaw (orientée opérateur)

Référence synthétique des sections de configuration les plus utiles et de leurs valeurs par défaut.

Dernière vérification : **21 février 2026**.

Chemin du fichier de configuration :

- `~/.zeroclaw/config.toml`

## Clés principales

| Clé | Défaut | Notes |
|---|---|---|
| `default_provider` | `openrouter` | ID provider ou alias |
| `default_model` | `anthropic/claude-sonnet-4-6` | modèle routé via le provider sélectionné |
| `default_temperature` | `0.7` | température du modèle |
| `api_url` | non défini | override de base URL provider (ex: endpoint Ollama distant, ou base URL proxy OAuth OpenAI Codex) |

## `[agent]`

| Clé | Défaut | Rôle |
|---|---|---|
| `max_tool_iterations` | `10` | nombre max de tours de boucle tool-call par message utilisateur (CLI, gateway, channels) |

Notes :

- `max_tool_iterations = 0` retombe sur la valeur sûre `10`.
- Si un message channel dépasse cette limite, le runtime renvoie : `Agent exceeded maximum tool iterations (<value>)`.

## `[gateway]`

| Clé | Défaut | Rôle |
|---|---|---|
| `host` | `127.0.0.1` | adresse de bind |
| `port` | `3000` | port d'écoute gateway |
| `require_pairing` | `true` | impose le pairing avant auth bearer |
| `allow_public_bind` | `false` | évite l'exposition publique accidentelle |

## `[memory]`

| Clé | Défaut | Rôle |
|---|---|---|
| `backend` | `sqlite` | `sqlite`, `lucid`, `markdown`, `none` |
| `auto_save` | `true` | persistance automatique |
| `embedding_provider` | `none` | `none`, `openai` ou endpoint custom |
| `vector_weight` | `0.7` | poids vectoriel pour le ranking hybride |
| `keyword_weight` | `0.3` | poids mots-clés pour le ranking hybride |

## `[channels_config]`

Les options channel de haut niveau se configurent dans `channels_config`.

| Clé | Défaut | Rôle |
|---|---|---|
| `message_timeout_secs` | `300` | timeout (secondes) pour traiter un message channel (LLM + tools) |

Exemples :

- `[channels_config.telegram]`
- `[channels_config.discord]`
- `[channels_config.whatsapp]`
- `[channels_config.email]`

Notes :

- Le défaut `300s` est optimisé pour les LLM on-device (Ollama), plus lents que les APIs cloud.
- Avec des APIs cloud (OpenAI, Anthropic, etc.), vous pouvez descendre à `60` ou moins.
- Les valeurs inférieures à `30` sont clampées à `30` pour éviter les timeouts immédiats en boucle.
- En cas de timeout, l'utilisateur reçoit : `⚠️ Request timed out while waiting for the model. Please try again.`

Voir la matrice channels et la sémantique d'allowlist dans [channels-reference.md](channels-reference.md).

## Défauts de sécurité importants

- allowlists channel en deny-by-default (`[]` = tout refuser)
- pairing gateway activé par défaut
- bind public désactivé par défaut

## Commandes de validation

Après modification de config :

```bash
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
```

## Docs liées

- [channels-reference.md](channels-reference.md)
- [providers-reference.md](providers-reference.md)
- [operations-runbook.md](operations-runbook.md)
- [troubleshooting.fr.md](troubleshooting.fr.md)
