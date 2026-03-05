# Frontend ZeroClaw per GitHub Pages (Vite)

Questo è il frontend standalone per GitHub Pages.

Lingue: [English](README.md) | [Español](README.es.md) | [Português](README.pt.md) | **Italiano**

## Comandi

```bash
cd site
npm install
npm run dev
```

Build per GitHub Pages:

```bash
cd site
npm run build
```

L'output di build viene generato in:

```text
/home/ubuntu/zeroclaw/gh-pages
```

Note:

- La directory di output è intenzionalmente `gh-pages/` (non `out/`).
- La base di Vite è configurata su `/zeroclaw/` per `https://zeroclaw-labs.github.io/zeroclaw/`.
- I link alla documentazione nella UI puntano alle pagine docs renderizzate su GitHub per lettura diretta.
- Il navigatore documentazione supporta:
  - ricerca per parole chiave con ranking pesato
  - filtri per categoria e livello (`Core` / `Advanced`)
  - scorciatoie tastiera: `/` per mettere a fuoco la ricerca, `Esc` per azzerare i filtri
- "Quick Start Paths" offre flussi guidati per onboarding, canali e hardening.
- La command palette è abilitata:
  - apertura con `Ctrl/Cmd + K`
  - include azioni rapide (vai a docs, repository, cambio tema/lingua)
  - include voci di ricerca fuzzy nella documentazione
  - supporta navigazione da tastiera (`↑` / `↓` / `Enter`) con evidenziazione elemento attivo
  - supporta ciclo con `Tab` / `Shift+Tab` e pannello preview live (desktop)
- Il sistema temi è abilitato:
  - `Auto` / `Dark` / `Light`
  - preferenza salvata in `localStorage`
- i18n è abilitato:
  - la UI attualmente supporta `English` e `简体中文`
  - sono disponibili traduzioni aggiuntive di questo README in `Español`, `Português` e `Italiano`
  - la preferenza lingua viene salvata in `localStorage`
  - il parametro lingua URL (`?lang=en` / `?lang=zh`) viene sincronizzato per link condivisibili
- Il sistema responsive è stato esteso:
  - breakpoints migliorati per desktop/tablet/mobile
  - controlli topbar e layout pannelli adattivi
  - container query usata per modalità compatta delle doc-card
  - section rail desktop + quick dock mobile per navigazione più rapida su pagine lunghe

## Deploy

Il repository include questo workflow:

```text
.github/workflows/pages-deploy.yml
```

Comportamento:

- Trigger su push a `main` quando cambiano `site/**`, `docs/**` o `README.md`.
- Il build gira in `site/` e pubblica l'artefatto da `gh-pages/`.
- Deploy con le action ufficiali di GitHub Pages.
