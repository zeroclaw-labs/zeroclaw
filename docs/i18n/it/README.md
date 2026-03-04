<p align="center">
  <img src="../../../zeroclaw.png" alt="ZeroClaw" width="200" />
</p>

<h1 align="center">ZeroClaw 🦀</h1>

<p align="center">
  <strong>Zero overhead. Zero compromesso. 100% Rust. 100% Agnostico.</strong><br>
  ⚡️ <strong>Funziona su qualsiasi hardware con <5MB RAM: 99% meno memoria di OpenClaw e 98% più economico di un Mac mini!</strong>
</p>

<p align="center">
  <a href="../../../LICENSE-APACHE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache%202.0-blue.svg" alt="Licenza: MIT OR Apache-2.0" /></a>
  <a href="../../../NOTICE"><img src="https://img.shields.io/github/contributors/zeroclaw-labs/zeroclaw?color=green" alt="Contributori" /></a>
  <a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=flat&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>
  <a href="https://x.com/zeroclawlabs?s=21"><img src="https://img.shields.io/badge/X-%40zeroclawlabs-000000?style=flat&logo=x&logoColor=white" alt="X: @zeroclawlabs" /></a>
  <a href="https://zeroclawlabs.cn/group.jpg"><img src="https://img.shields.io/badge/WeChat-Group-B7D7A8?logo=wechat&logoColor=white" alt="Gruppo WeChat" /></a>
  <a href="https://t.me/zeroclawlabs"><img src="https://img.shields.io/badge/Telegram-%40zeroclawlabs-26A5E4?style=flat&logo=telegram&logoColor=white" alt="Telegram: @zeroclawlabs" /></a>
  <a href="https://www.facebook.com/groups/zeroclaw"><img src="https://img.shields.io/badge/Facebook-Group-1877F2?style=flat&logo=facebook&logoColor=white" alt="Gruppo Facebook" /></a>
  <a href="https://www.reddit.com/r/zeroclawlabs/"><img src="https://img.shields.io/badge/Reddit-r%2Fzeroclawlabs-FF4500?style=flat&logo=reddit&logoColor=white" alt="Reddit: r/zeroclawlabs" /></a>
</p>

<p align="center">
Sviluppato da studenti e membri delle comunità Harvard, MIT e Sundai.Club.
</p>

<p align="center">
  🌐 <strong>Lingue:</strong> <a href="../../../README.md">English</a> · <a href="../zh-CN/README.md">简体中文</a> · <a href="../es/README.md">Español</a> · <a href="../pt/README.md">Português</a> · <a href="README.md">Italiano</a> · <a href="../ja/README.md">日本語</a> · <a href="../ru/README.md">Русский</a> · <a href="../fr/README.md">Français</a> · <a href="../vi/README.md">Tiếng Việt</a> · <a href="../el/README.md">Ελληνικά</a>
</p>

<p align="center">
  <strong>Framework veloce, piccolo e completamente autonomo</strong><br />
  Distribuisci ovunque. Scambia qualsiasi cosa.
</p>

<p align="center">
  ZeroClaw è il <strong>framework runtime</strong> per workflow agentici — infrastruttura che astrae modelli, strumenti, memoria ed esecuzione così gli agenti possono essere costruiti una volta ed eseguiti ovunque.
</p>

<p align="center"><code>Architettura basata su trait · runtime sicuro per impostazione predefinita · provider/canale/strumento scambiabile · tutto collegabile</code></p>

### ✨ Caratteristiche

- 🏎️ **Runtime Leggero per Impostazione Predefinita:** I comuni workflow CLI e di stato vengono eseguiti in un envelope di memoria di pochi megabyte nelle build di release.
- 💰 **Distribuzione Economica:** Progettato per schede economiche e piccole istanze cloud senza dipendenze di runtime pesanti.
- ⚡ **Avvii a Freddo Rapidi:** Il runtime Rust a singolo binario mantiene l'avvio di comandi e daemon quasi istantaneo per le operazioni quotidiane.
- 🌍 **Architettura Portatile:** Un workflow binary-first attraverso ARM, x86 e RISC-V con provider/canali/strumenti scambiabili.
- 🔍 **Fase di Ricerca:** Raccolta proattiva di informazioni attraverso gli strumenti prima della generazione della risposta — riduce le allucinazioni verificando prima i fatti.

### Perché i team scelgono ZeroClaw

- **Leggero per impostazione predefinita:** binario Rust piccolo, avvio rapido, footprint di memoria basso.
- **Sicuro per design:** pairing, sandboxing rigoroso, liste di permessi esplicite, scope del workspace.
- **Completamente scambiabile:** i sistemi core sono trait (provider, canali, strumenti, memoria, tunnel).
- **Nessun lock-in:** supporto provider compatibile con OpenAI + endpoint personalizzati collegabili.

## Avvio Rapido

### Opzione 1: Homebrew (macOS/Linuxbrew)

```bash
brew install zeroclaw
```

### Opzione 2: Clona + Bootstrap

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./bootstrap.sh
```

> **Nota:** Le build da sorgente richiedono ~2GB RAM e ~6GB disco. Per sistemi con risorse limitate, usa `./bootstrap.sh --prefer-prebuilt` per scaricare un binario precompilato.

### Opzione 3: Cargo Install

```bash
cargo install zeroclaw
```

### Prima Esecuzione

```bash
# Avvia il gateway (serve l'API/UI della Dashboard Web)
zeroclaw gateway

# Apri l'URL del dashboard mostrata nei log di avvio
# (default: http://127.0.0.1:3000/)

# O chatta direttamente
zeroclaw chat "Ciao!"
```

Per opzioni di configurazione dettagliate, consulta [docs/one-click-bootstrap.md](../../../docs/one-click-bootstrap.md).

---

## ⚠️ Repository Ufficiale e Avviso di Impersonazione

**Questo è l'unico repository ufficiale di ZeroClaw:**

> https://github.com/zeroclaw-labs/zeroclaw

Qualsiasi altro repository, organizzazione, dominio o pacchetto che affermi di essere "ZeroClaw" o implichi affiliazione con ZeroClaw Labs **non è autorizzato e non è affiliato con questo progetto**.

Se incontri impersonazione o uso improprio del marchio, per favore [apri una issue](https://github.com/zeroclaw-labs/zeroclaw/issues).

---

## Licenza

ZeroClaw è con doppia licenza per massima apertura e protezione dei contributori:

| Licenza | Caso d'uso |
|---|---|
| [MIT](../../../LICENSE-MIT) | Open-source, ricerca, accademico, uso personale |
| [Apache 2.0](../../../LICENSE-APACHE) | Protezione brevetti, istituzionale, distribuzione commerciale |

Puoi scegliere qualsiasi licenza. **I contributori concedono automaticamente diritti sotto entrambe** — consulta [CLA.md](../../../CLA.md) per l'accordo completo dei contributori.

## Contribuire

Consulta [CONTRIBUTING.md](../../../CONTRIBUTING.md) e [CLA.md](../../../CLA.md). Implementa un trait, invia un PR.

---

**ZeroClaw** — Zero overhead. Zero compromesso. Distribuisci ovunque. Scambia qualsiasi cosa. 🦀

---

## Star History

<p align="center">
  <a href="https://www.star-history.com/#zeroclaw-labs/zeroclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
