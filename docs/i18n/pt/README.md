<p align="center">
  <img src="../../../zeroclaw.png" alt="ZeroClaw" width="200" />
</p>

<h1 align="center">ZeroClaw 🦀</h1>

<p align="center">
  <strong>Sobrecarga zero. Compromisso zero. 100% Rust. 100% Agnóstico.</strong><br>
  ⚡️ <strong>Funciona em qualquer hardware com <5MB RAM: 99% menos memória que OpenClaw e 98% mais barato que um Mac mini!</strong>
</p>

<p align="center">
  <a href="../../../LICENSE-APACHE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache%202.0-blue.svg" alt="Licença: MIT OR Apache-2.0" /></a>
  <a href="../../../NOTICE"><img src="https://img.shields.io/github/contributors/zeroclaw-labs/zeroclaw?color=green" alt="Contribuidores" /></a>
  <a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=flat&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>
  <a href="https://x.com/zeroclawlabs?s=21"><img src="https://img.shields.io/badge/X-%40zeroclawlabs-000000?style=flat&logo=x&logoColor=white" alt="X: @zeroclawlabs" /></a>
  <a href="https://zeroclawlabs.cn/group.jpg"><img src="https://img.shields.io/badge/WeChat-Group-B7D7A8?logo=wechat&logoColor=white" alt="Grupo WeChat" /></a>
  <a href="https://t.me/zeroclawlabs"><img src="https://img.shields.io/badge/Telegram-%40zeroclawlabs-26A5E4?style=flat&logo=telegram&logoColor=white" alt="Telegram: @zeroclawlabs" /></a>
  <a href="https://www.facebook.com/groups/zeroclaw"><img src="https://img.shields.io/badge/Facebook-Group-1877F2?style=flat&logo=facebook&logoColor=white" alt="Grupo Facebook" /></a>
  <a href="https://www.reddit.com/r/zeroclawlabs/"><img src="https://img.shields.io/badge/Reddit-r%2Fzeroclawlabs-FF4500?style=flat&logo=reddit&logoColor=white" alt="Reddit: r/zeroclawlabs" /></a>
</p>

<p align="center">
Desenvolvido por estudantes e membros das comunidades de Harvard, MIT e Sundai.Club.
</p>

<p align="center">
  🌐 <strong>Idiomas:</strong> <a href="../../../README.md">English</a> · <a href="../zh-CN/README.md">简体中文</a> · <a href="../es/README.md">Español</a> · <a href="README.md">Português</a> · <a href="../it/README.md">Italiano</a> · <a href="../ja/README.md">日本語</a> · <a href="../ru/README.md">Русский</a> · <a href="../fr/README.md">Français</a> · <a href="../vi/README.md">Tiếng Việt</a> · <a href="../el/README.md">Ελληνικά</a>
</p>

<p align="center">
  <strong>Framework rápido, pequeno e totalmente autônomo</strong><br />
  Implante em qualquer lugar. Troque qualquer coisa.
</p>

<p align="center">
  ZeroClaw é o <strong>framework de runtime</strong> para fluxos de trabalho agentes — infraestrutura que abstrai modelos, ferramentas, memória e execução para que agentes possam ser construídos uma vez e executados em qualquer lugar.
</p>

<p align="center"><code>Arquitetura baseada em traits · runtime seguro por padrão · provedor/canal/ferramenta trocável · tudo conectável</code></p>

### ✨ Características

- 🏎️ **Runtime Enxuto por Padrão:** Fluxos de trabalho comuns de CLI e status rodam em um envelope de memória de poucos megabytes em builds de release.
- 💰 **Implantação Econômica:** Projetado para placas de baixo custo e instâncias cloud pequenas sem dependências de runtime pesadas.
- ⚡ **Inícios a Frio Rápidos:** Runtime Rust de binário único mantém inicialização de comandos e daemon quase instantânea para operações diárias.
- 🌍 **Arquitetura Portátil:** Um fluxo de trabalho binary-first através de ARM, x86 e RISC-V com provedores/canais/ferramentas trocáveis.
- 🔍 **Fase de Pesquisa:** Coleta proativa de informações através de ferramentas antes da geração de resposta — reduz alucinações verificando fatos primeiro.

### Por que as equipes escolhem ZeroClaw

- **Enxuto por padrão:** binário Rust pequeno, inicialização rápida, pegada de memória baixa.
- **Seguro por design:** pareamento, sandboxing estrito, listas de permitidos explícitas, escopo de workspace.
- **Totalmente trocável:** sistemas principais são traits (provedores, canais, ferramentas, memória, túneis).
- **Sem lock-in:** suporte de provedor compatível com OpenAI + endpoints personalizados conectáveis.

## Início Rápido

### Opção 1: Homebrew (macOS/Linuxbrew)

```bash
brew install zeroclaw
```

### Opção 2: Clonar + Bootstrap

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./bootstrap.sh
```

> **Nota:** Builds a partir do fonte requerem ~2GB RAM e ~6GB disco. Para sistemas com recursos limitados, use `./bootstrap.sh --prefer-prebuilt` para baixar um binário pré-compilado.

### Opção 3: Cargo Install

```bash
cargo install zeroclaw
```

### Primeira Execução

```bash
# Iniciar o gateway (serve o API/UI do Dashboard Web)
zeroclaw gateway

# Abrir a URL do dashboard mostrada nos logs de inicialização
# (por padrão: http://127.0.0.1:3000/)

# Ou conversar diretamente
zeroclaw chat "Olá!"
```

Para opções de configuração detalhadas, consulte [docs/one-click-bootstrap.md](../../../docs/one-click-bootstrap.md).

---

## ⚠️ Repositório Oficial e Aviso de Representação

**Este é o único repositório oficial do ZeroClaw:**

> https://github.com/zeroclaw-labs/zeroclaw

Qualquer outro repositório, organização, domínio ou pacote que afirme ser "ZeroClaw" ou implique afiliação com ZeroClaw Labs **não está autorizado e não é afiliado com este projeto**.

Se você encontrar representação ou uso indevido de marca, por favor [abra uma issue](https://github.com/zeroclaw-labs/zeroclaw/issues).

---

## Licença

ZeroClaw tem licença dupla para máxima abertura e proteção de contribuidores:

| Licença | Caso de uso |
|---|---|
| [MIT](../../../LICENSE-MIT) | Open-source, pesquisa, acadêmico, uso pessoal |
| [Apache 2.0](../../../LICENSE-APACHE) | Proteção de patentes, institucional, implantação comercial |

Você pode escolher qualquer uma das licenças. **Os contribuidores concedem automaticamente direitos sob ambas** — consulte [CLA.md](../../../CLA.md) para o acordo completo de contribuidor.

## Contribuindo

Consulte [CONTRIBUTING.md](../../../CONTRIBUTING.md) e [CLA.md](../../../CLA.md). Implemente uma trait, envie um PR.

---

**ZeroClaw** — Sobrecarga zero. Compromisso zero. Implante em qualquer lugar. Troque qualquer coisa. 🦀

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
