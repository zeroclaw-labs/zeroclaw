# Frontend de ZeroClaw para GitHub Pages (Vite)

Este es el frontend independiente para GitHub Pages.

Idiomas: [English](README.md) | **Español** | [Português](README.pt.md) | [Italiano](README.it.md)

## Comandos

```bash
cd site
npm install
npm run dev
```

Compilar para GitHub Pages:

```bash
cd site
npm run build
```

La salida de compilación se genera en:

```text
/home/ubuntu/zeroclaw/gh-pages
```

Notas:

- El directorio de salida es intencionalmente `gh-pages/` (no `out/`).
- La base de Vite está configurada como `/zeroclaw/` para `https://zeroclaw-labs.github.io/zeroclaw/`.
- Los enlaces de documentación en la UI apuntan a páginas renderizadas de GitHub Docs para lectura directa.
- El navegador de documentación soporta:
  - búsqueda por palabras clave con ranking ponderado
  - filtros por categoría y nivel (`Core` / `Advanced`)
  - atajos de teclado: `/` para enfocar la búsqueda y `Esc` para restablecer filtros
- "Quick Start Paths" ofrece flujos guiados por tareas para onboarding, canales y hardening.
- La paleta de comandos está habilitada:
  - abrir con `Ctrl/Cmd + K`
  - incluye acciones rápidas (ir a docs, repositorio, cambiar tema/idioma)
  - incluye entradas de búsqueda difusa en documentación
  - soporta navegación por teclado (`↑` / `↓` / `Enter`) con resaltado del elemento activo
  - soporta ciclo con `Tab` / `Shift+Tab` y panel de vista previa en vivo (escritorio)
- El sistema de tema está habilitado:
  - `Auto` / `Dark` / `Light`
  - preferencia persistida en `localStorage`
- i18n está habilitado:
  - la UI actualmente soporta `English` y `简体中文`
  - hay traducciones adicionales de este README en `Español`, `Português` e `Italiano`
  - la preferencia de idioma se guarda en `localStorage`
  - el parámetro de idioma en URL (`?lang=en` / `?lang=zh`) se sincroniza para enlaces compartibles
- El sistema responsive fue ampliado:
  - mejores breakpoints para desktop/tablet/móvil
  - controles adaptativos en la barra superior y paneles
  - uso de container query para modo compacto en tarjetas de documentación
  - section rail en desktop + quick dock móvil para navegación más rápida en páginas largas

## Despliegue

El repositorio incluye este workflow:

```text
.github/workflows/pages-deploy.yml
```

Comportamiento:

- Se dispara en pushes a `main` cuando cambian `site/**`, `docs/**` o `README.md`.
- El build se ejecuta en `site/` y publica artefacto desde `gh-pages/`.
- Despliega con acciones oficiales de GitHub Pages.
