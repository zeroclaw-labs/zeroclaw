# Frontend do ZeroClaw para GitHub Pages (Vite)

Este é o frontend independente para GitHub Pages.

Idiomas: [English](README.md) | [Español](README.es.md) | **Português** | [Italiano](README.it.md)

## Comandos

```bash
cd site
npm install
npm run dev
```

Build para GitHub Pages:

```bash
cd site
npm run build
```

A saída do build é gerada em:

```text
/home/ubuntu/zeroclaw/gh-pages
```

Notas:

- O diretório de saída é intencionalmente `gh-pages/` (não `out/`).
- A base do Vite está configurada para `/zeroclaw/` em `https://zeroclaw-labs.github.io/zeroclaw/`.
- Os links de documentação na UI apontam para páginas renderizadas do GitHub Docs para leitura direta.
- O navegador de documentação suporta:
  - busca por palavras-chave com ranking ponderado
  - filtros por categoria e nível (`Core` / `Advanced`)
  - atalhos de teclado: `/` para focar a busca e `Esc` para limpar filtros
- "Quick Start Paths" fornece fluxos por tarefa para onboarding, canais e hardening.
- A paleta de comandos está habilitada:
  - abrir com `Ctrl/Cmd + K`
  - inclui ações rápidas (ir para docs, repositório, trocar tema/idioma)
  - inclui entradas de busca fuzzy para documentação
  - suporta navegação por teclado (`↑` / `↓` / `Enter`) com destaque do item ativo
  - suporta navegação com `Tab` / `Shift+Tab` e painel de preview ao vivo (desktop)
- O sistema de tema está habilitado:
  - `Auto` / `Dark` / `Light`
  - preferência persistida em `localStorage`
- i18n está habilitado:
  - a UI atualmente suporta `English` e `简体中文`
  - traduções adicionais deste README estão disponíveis em `Español`, `Português` e `Italiano`
  - a preferência de idioma é persistida em `localStorage`
  - o parâmetro de idioma na URL (`?lang=en` / `?lang=zh`) é sincronizado para links compartilháveis
- O sistema responsivo foi aprofundado:
  - breakpoints melhorados para desktop/tablet/mobile
  - controles adaptativos na topbar e layouts de painéis
  - uso de container query para modo compacto dos cards de docs
  - section rail no desktop + quick dock no mobile para navegação mais rápida em páginas longas

## Deploy

O repositório inclui este workflow:

```text
.github/workflows/pages-deploy.yml
```

Comportamento:

- Dispara em pushes para `main` quando `site/**`, `docs/**` ou `README.md` mudam.
- O build roda em `site/` e publica o artefato a partir de `gh-pages/`.
- Faz deploy usando as actions oficiais do GitHub Pages.
