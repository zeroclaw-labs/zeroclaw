// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

(() => {
    // Resolve dark/light from the active theme's --color-scheme, which
    // pc-themes.css sets per dashboard theme (html.<id>). This works for all
    // themes without hardcoding their names; falls back to the OS preference.
    function isLight() {
        const scheme = getComputedStyle(document.documentElement)
            .getPropertyValue('--color-scheme')
            .trim();
        if (scheme === 'light') return true;
        if (scheme === 'dark') return false;
        return !window.matchMedia('(prefers-color-scheme: dark)').matches;
    }

    function cssVar(name, fallback) {
        const v = getComputedStyle(document.documentElement)
            .getPropertyValue(name)
            .trim();
        return v || fallback;
    }

    const light = isLight();

    // Theme mermaid from our --pc-* tokens so nodes/edges/text track the
    // active dashboard theme and stay legible on any background.
    mermaid.initialize({
        startOnLoad: true,
        theme: 'base',
        themeVariables: {
            fontSize: '18px',
            fontFamily: cssVar('--pc-font-ui', 'ui-sans-serif, system-ui, sans-serif'),
            background: cssVar('--pc-bg-base', light ? '#ffffff' : '#1e1e24'),
            primaryColor: cssVar('--pc-bg-elevated', light ? '#eef2f7' : '#27272a'),
            secondaryColor: cssVar('--pc-bg-surface', light ? '#f4f4f5' : '#232329'),
            tertiaryColor: cssVar('--pc-bg-surface', light ? '#f4f4f5' : '#232329'),
            primaryTextColor: cssVar('--pc-text-primary', light ? '#18181b' : '#d4d4d8'),
            secondaryTextColor: cssVar('--pc-text-primary', light ? '#18181b' : '#d4d4d8'),
            tertiaryTextColor: cssVar('--pc-text-primary', light ? '#18181b' : '#d4d4d8'),
            primaryBorderColor: cssVar('--pc-accent', light ? '#0891b2' : '#22d3ee'),
            secondaryBorderColor: cssVar('--pc-border-strong', light ? '#00000022' : '#ffffff22'),
            tertiaryBorderColor: cssVar('--pc-border-strong', light ? '#00000022' : '#ffffff22'),
            lineColor: cssVar('--pc-accent', light ? '#0891b2' : '#22d3ee'),
            textColor: cssVar('--pc-text-primary', light ? '#18181b' : '#d4d4d8'),
            nodeTextColor: cssVar('--pc-text-primary', light ? '#18181b' : '#d4d4d8'),
        },
        flowchart: {
            curve: 'basis',
            padding: 20,
            nodeSpacing: 50,
            rankSpacing: 60,
        },
        sequence: {
            actorFontSize: 16,
            noteFontSize: 14,
            messageFontSize: 14,
            diagramMarginX: 30,
            diagramMarginY: 30,
            boxMargin: 12,
        },
    });

    // Mermaid renders to static SVG, so switching theme needs a re-render.
    // Reload when the active scheme actually flips (light <-> dark) after a
    // theme-switcher click. Works for every theme via the delegated handler.
    const themeList = document.getElementById('mdbook-theme-list');
    if (themeList) {
        themeList.addEventListener('click', (e) => {
            if (!e.target.closest('button.theme')) return;
            // Defer so book.js applies the new html class first.
            setTimeout(() => {
                if (isLight() !== light) window.location.reload();
            }, 60);
        });
    }
})();
