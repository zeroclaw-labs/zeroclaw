# Référence des commandes ZeroClaw

Cette référence est dérivée de la surface CLI actuelle (`zeroclaw --help`).

Dernière vérification : **21 février 2026**.

## Commandes de premier niveau

| Commande | But |
|---|---|
| `onboard` | Initialiser rapidement l'espace de travail / la configuration (assisté ou interactif) |
| `agent` | Exécuter un chat interactif ou un mode message unique |
| `gateway` | Démarrer la passerelle HTTP (webhook + WhatsApp) |
| `daemon` | Démarrer le runtime supervisé (gateway + channels + heartbeat/scheduler optionnels) |
| `service` | Gérer le cycle de vie du service utilisateur OS |
| `doctor` | Lancer diagnostics et vérifications de fraîcheur |
| `status` | Afficher le résumé configuration + système |
| `cron` | Gérer les tâches planifiées |
| `models` | Rafraîchir les catalogues de modèles providers |
| `providers` | Lister IDs provider, alias et provider actif |
| `channel` | Gérer les channels et leurs checks de santé |
| `integrations` | Inspecter les détails d'intégration |
| `skills` | Lister / installer / supprimer des skills |
| `migrate` | Importer depuis des runtimes externes (actuellement OpenClaw) |
| `hardware` | Découvrir et inspecter le matériel USB |
| `peripheral` | Configurer et flasher les périphériques |

## Groupes de commandes

### `onboard`

- `zeroclaw onboard`
- `zeroclaw onboard --interactive`
- `zeroclaw onboard --channels-only`
- `zeroclaw onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`

### `agent`

- `zeroclaw agent`
- `zeroclaw agent -m "Hello"`
- `zeroclaw agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `zeroclaw agent --peripheral <board:path>`

### `gateway` / `daemon`

- `zeroclaw gateway [--host <HOST>] [--port <PORT>]`
- `zeroclaw daemon [--host <HOST>] [--port <PORT>]`

### `service`

- `zeroclaw service install`
- `zeroclaw service start`
- `zeroclaw service stop`
- `zeroclaw service status`
- `zeroclaw service uninstall`

### `cron`

- `zeroclaw cron list`
- `zeroclaw cron add <expr> [--tz <IANA_TZ>] <command>`
- `zeroclaw cron add-at <rfc3339_timestamp> <command>`
- `zeroclaw cron add-every <every_ms> <command>`
- `zeroclaw cron once <delay> <command>`
- `zeroclaw cron remove <id>`
- `zeroclaw cron pause <id>`
- `zeroclaw cron resume <id>`

### `models`

- `zeroclaw models refresh`
- `zeroclaw models refresh --provider <ID>`
- `zeroclaw models refresh --force`

`models refresh` prend actuellement en charge le rafraîchissement live pour les provider IDs : `openrouter`, `openai`, `anthropic`, `groq`, `mistral`, `deepseek`, `xai`, `together-ai`, `gemini`, `ollama`, `astrai`, `venice`, `fireworks`, `cohere`, `moonshot`, `glm`, `zai`, `qwen` et `nvidia`.

### `channel`

- `zeroclaw channel list`
- `zeroclaw channel start`
- `zeroclaw channel doctor`
- `zeroclaw channel bind-telegram <IDENTITY>`
- `zeroclaw channel add <type> <json>`
- `zeroclaw channel remove <name>`

Commandes runtime dans le chat (Telegram / Discord quand le serveur channel tourne) :

- `/models`
- `/models <provider>`
- `/model`
- `/model <model-id>`

`add/remove` redirige aujourd'hui vers les parcours setup géré / configuration manuelle (pas encore des mutateurs déclaratifs complets).

### `integrations`

- `zeroclaw integrations info <name>`

### `skills`

- `zeroclaw skills list`
- `zeroclaw skills install <source>`
- `zeroclaw skills remove <name>`

### `migrate`

- `zeroclaw migrate openclaw [--source <path>] [--dry-run]`

### `hardware`

- `zeroclaw hardware discover`
- `zeroclaw hardware introspect <path>`
- `zeroclaw hardware info [--chip <chip_name>]`

### `peripheral`

- `zeroclaw peripheral list`
- `zeroclaw peripheral add <board> <path>`
- `zeroclaw peripheral flash [--port <serial_port>]`
- `zeroclaw peripheral setup-uno-q [--host <ip_or_host>]`
- `zeroclaw peripheral flash-nucleo`

## Astuce de validation

Pour vérifier rapidement la doc avec votre binaire local :

```bash
zeroclaw --help
zeroclaw <commande> --help
```
