<p align="center">
  <img src="zeroclaw.png" alt="ZeroClaw" width="200" />
</p>

<h1 align="center">ZeroClaw 🦀 (Français)</h1>

<p align="center">
  <strong>Zéro overhead, zéro compromis ; déployez partout, remplacez tout.</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
  <a href="NOTICE"><img src="https://img.shields.io/badge/contributors-27+-green.svg" alt="Contributors" /></a>
  <a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=flat&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>
  <a href="https://zeroclawlabs.cn/group.jpg"><img src="https://img.shields.io/badge/WeChat-Group-B7D7A8?logo=wechat&logoColor=white" alt="WeChat Group" /></a>
  <a href="https://x.com/zeroclawlabs?s=21"><img src="https://img.shields.io/badge/X-%40zeroclawlabs-000000?style=flat&logo=x&logoColor=white" alt="X: @zeroclawlabs" /></a>
  <a href="https://www.xiaohongshu.com/user/profile/67cbfc43000000000d008307?xsec_token=AB73VnYnGNx5y36EtnnZfGmAmS-6Wzv8WMuGpfwfkg6Yc%3D&xsec_source=pc_search"><img src="https://img.shields.io/badge/Xiaohongshu-Official-FF2442?style=flat" alt="Xiaohongshu: Official" /></a>
  <a href="https://t.me/zeroclawlabs"><img src="https://img.shields.io/badge/Telegram-%40zeroclawlabs-26A5E4?style=flat&logo=telegram&logoColor=white" alt="Telegram: @zeroclawlabs" /></a>
  <a href="https://t.me/zeroclawlabs_cn"><img src="https://img.shields.io/badge/Telegram%20CN-%40zeroclawlabs__cn-26A5E4?style=flat&logo=telegram&logoColor=white" alt="Telegram CN: @zeroclawlabs_cn" /></a>
  <a href="https://t.me/zeroclawlabs_ru"><img src="https://img.shields.io/badge/Telegram%20RU-%40zeroclawlabs__ru-26A5E4?style=flat&logo=telegram&logoColor=white" alt="Telegram RU: @zeroclawlabs_ru" /></a>
  <a href="https://www.reddit.com/r/zeroclawlabs/"><img src="https://img.shields.io/badge/Reddit-r%2Fzeroclawlabs-FF4500?style=flat&logo=reddit&logoColor=white" alt="Reddit: r/zeroclawlabs" /></a>
</p>

<p align="center">
  🌐 Langues : <a href="README.md">English</a> · <a href="README.zh-CN.md">简体中文</a> · <a href="README.ja.md">日本語</a> · <a href="README.ru.md">Русский</a> · <a href="README.fr.md">Français</a> · <a href="README.vi.md">Tiếng Việt</a>
</p>

<p align="center">
  <a href="bootstrap.sh">Installation en un clic</a> |
  <a href="docs/getting-started/README.md">Prise en main</a> |
  <a href="docs/README.fr.md">Hub de documentation</a> |
  <a href="docs/SUMMARY.md">Sommaire docs</a>
</p>

<p align="center">
  <strong>Accès rapide :</strong>
  <a href="docs/reference/README.md">Références</a> ·
  <a href="docs/operations/README.md">Opérations</a> ·
  <a href="docs/troubleshooting.fr.md">Dépannage</a> ·
  <a href="docs/security/README.md">Sécurité</a> ·
  <a href="docs/hardware/README.md">Matériel</a> ·
  <a href="docs/contributing/README.md">Contribution & CI</a>
</p>

> Ce document est une traduction alignée de `README.md`, orientée lisibilité et exactitude (pas une traduction mot-à-mot).
>
> Les identifiants techniques (commandes, clés de configuration, chemins API, noms de Trait) restent en anglais pour éviter toute dérive sémantique.
>
> Dernière synchronisation : **2026-02-21**.

## 📢 Annonces

Ce tableau est réservé aux annonces importantes (breaking changes, alertes sécurité, fenêtres de maintenance, blocages de release).

| Date (UTC) | Niveau | Annonce | Action |
|---|---|---|---|
| 2026-02-19 | _Critique_ | Nous ne sommes **pas affiliés** à `openagen/zeroclaw` ni à `zeroclaw.org`. Le domaine `zeroclaw.org` pointe actuellement vers le fork `openagen/zeroclaw`, et ce domaine/référentiel usurpe notre site/projet officiel. | Ne faites pas confiance aux informations, binaires, levées de fonds ou annonces provenant de ces sources. Référez-vous uniquement à ce dépôt et à nos comptes sociaux vérifiés. |
| 2026-02-21 | _Important_ | Notre site officiel est désormais en ligne : [zeroclawlabs.ai](https://zeroclawlabs.ai). Merci pour votre patience pendant cette attente. Nous constatons toujours des tentatives d'usurpation : ne participez à aucune activité d'investissement/financement au nom de ZeroClaw si elle n'est pas publiée via nos canaux officiels. | Pour les annonces et la vérification des informations, fiez-vous uniquement à ce dépôt et à nos comptes sociaux vérifiés. |
| 2026-02-19 | _Important_ | Anthropic a mis à jour la clause Authentication and Credential Use le 2026-02-19. L'authentification OAuth (Free/Pro/Max) est réservée à Claude Code et Claude.ai ; l'usage de tokens OAuth Claude Free/Pro/Max dans d'autres produits/services (dont Agent SDK) n'est pas autorisé et peut violer les Consumer Terms of Service. | Par prudence, évitez temporairement les intégrations Claude Code OAuth. Clause originale : [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use). |

## Présentation

ZeroClaw est un runtime d'agents autonomes haute performance, sobre en ressources et composable :

- Implémentation Rust native, distribution en binaire unique, portable ARM / x86 / RISC-V
- Architecture pilotée par Traits (`Provider` / `Channel` / `Tool` / `Memory`) remplaçables
- Valeurs de sécurité par défaut : pairing, allowlist explicite, sandbox, contraintes de scope

## Pourquoi ZeroClaw

- **Runtime léger par défaut** : les workflows CLI et `status` restent généralement dans une enveloppe mémoire de quelques Mo.
- **Déploiement économique** : conçu pour des cartes peu coûteuses et des petites instances cloud.
- **Cold start rapide** : le binaire Rust unique permet un démarrage quasi immédiat.
- **Portable** : même workflow sur ARM / x86 / RISC-V avec providers/channels/tools interchangeables.

## Aperçu benchmark (ZeroClaw vs OpenClaw, reproductible)

Comparatif local rapide (macOS arm64, février 2026), normalisé pour du matériel edge 0.8GHz.

| | OpenClaw | NanoBot | PicoClaw | ZeroClaw 🦀 |
|---|---|---|---|---|
| **Langage** | TypeScript | Python | Go | **Rust** |
| **RAM** | > 1GB | > 100MB | < 10MB | **< 5MB** |
| **Démarrage (coeur 0.8GHz)** | > 500s | > 30s | < 1s | **< 10ms** |
| **Taille binaire** | ~28MB (dist) | N/A (scripts) | ~8MB | **3.4 MB** |
| **Coût** | Mac Mini $599 | Linux SBC ~$50 | Carte Linux $10 | **Matériel $10** |

> Note : les résultats ZeroClaw sont mesurés sur build release avec `/usr/bin/time -l`. OpenClaw dépend du runtime Node.js (souvent ~390MB mémoire additionnelle). NanoBot dépend du runtime Python. PicoClaw et ZeroClaw sont des binaires statiques.

<p align="center">
  <img src="zero-claw.jpeg" alt="Comparaison ZeroClaw vs OpenClaw" width="800" />
</p>

### Mesure locale reproductible

```bash
cargo build --release
ls -lh target/release/zeroclaw

/usr/bin/time -l target/release/zeroclaw --help
/usr/bin/time -l target/release/zeroclaw status
```

Exemples actuels (macOS arm64, 2026-02-18) :

- Binaire release : `8.8M`
- `zeroclaw --help` : ~`0.02s`, pic mémoire ~`3.9MB`
- `zeroclaw status` : ~`0.01s`, pic mémoire ~`4.1MB`

## Installation en un clic

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./bootstrap.sh
```

Initialisation complète possible : `./bootstrap.sh --install-system-deps --install-rust` (peut nécessiter `sudo`).

Détails : [`docs/one-click-bootstrap.md`](docs/one-click-bootstrap.md)

## Démarrage rapide

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
cargo build --release --locked
cargo install --path . --force --locked

zeroclaw onboard --api-key sk-... --provider openrouter
zeroclaw onboard --interactive

zeroclaw agent -m "Hello, ZeroClaw!"

# par défaut : 127.0.0.1:3000
zeroclaw gateway

zeroclaw daemon
```

## Sécurité par défaut (essentiel)

- Bind gateway par défaut : `127.0.0.1:3000`
- Pairing exigé par défaut : `require_pairing = true`
- Bind public refusé par défaut : `allow_public_bind = false`
- Sémantique allowlist channels :
  - `[]` => deny-by-default
  - `["*"]` => allow all (uniquement en connaissance de risque)

## Exemple de configuration

```toml
api_key = "sk-..."
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"
default_temperature = 0.7

[memory]
backend = "sqlite"             # sqlite | lucid | markdown | none
auto_save = true
embedding_provider = "none"    # none | openai | custom:https://...

[gateway]
host = "127.0.0.1"
port = 3000
require_pairing = true
allow_public_bind = false
```

## OpenAI Codex OAuth + proxy backend personnalisé

```toml
# ~/.zeroclaw/config.toml
default_provider = "openai-codex"
default_model = "gpt-5-codex"
api_url = "https://your-proxy.example.com/v1" # le runtime envoie /v1/responses
```

Variables d'environnement optionnelles :

- `ZEROCLAW_CODEX_BASE_URL` (URL de base ; `/responses` est ajouté automatiquement)
- `ZEROCLAW_CODEX_RESPONSES_URL` (URL endpoint complète)

## Navigation docs (recommandé)

- Hub docs (anglais) : [`docs/README.md`](docs/README.md)
- Sommaire unifié : [`docs/SUMMARY.md`](docs/SUMMARY.md)
- Hub docs (français) : [`docs/README.fr.md`](docs/README.fr.md)
- Référence commandes : [`docs/commands-reference.fr.md`](docs/commands-reference.fr.md)
- Référence configuration : [`docs/config-reference.fr.md`](docs/config-reference.fr.md)
- Référence providers : [`docs/providers-reference.md`](docs/providers-reference.md)
- Référence channels : [`docs/channels-reference.md`](docs/channels-reference.md)
- Runbook opérations : [`docs/operations-runbook.md`](docs/operations-runbook.md)
- Dépannage : [`docs/troubleshooting.fr.md`](docs/troubleshooting.fr.md)
- Inventaire docs : [`docs/docs-inventory.md`](docs/docs-inventory.md)
- Snapshot triage projet : [`docs/project-triage-snapshot-2026-02-18.md`](docs/project-triage-snapshot-2026-02-18.md)

## Contribution et licence

- Guide de contribution : [`CONTRIBUTING.md`](CONTRIBUTING.md)
- Workflow PR : [`docs/pr-workflow.md`](docs/pr-workflow.md)
- Reviewer playbook : [`docs/reviewer-playbook.md`](docs/reviewer-playbook.md)
- Licence : MIT (voir [`LICENSE`](LICENSE) et [`NOTICE`](NOTICE))

---

Pour les détails complets (architecture, commandes exhaustives, API, workflow de développement), utilisez la version source : [`README.md`](README.md).
