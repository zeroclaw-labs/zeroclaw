// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

(() => {
    const darkThemes = ['ayu', 'navy', 'coal'];
    const lightThemes = ['light', 'rust'];

    const classList = document.getElementsByTagName('html')[0].classList;

    let lastThemeWasLight = true;
    for (const cssClass of classList) {
        if (darkThemes.includes(cssClass)) {
            lastThemeWasLight = false;
            break;
        }
    }

    const theme = lastThemeWasLight ? 'default' : 'dark';
    // Accessibility: larger default font and high-contrast strokes so diagrams
    // stay legible for readers with low vision or colourblindness. Values tuned
    // for both the ayu dark and light mdbook themes.
    mermaid.initialize({
        startOnLoad: true,
        theme,
        themeVariables: {
            fontSize: '18px',
            fontFamily: 'ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif',
            primaryBorderColor: lastThemeWasLight ? '#1f2937' : '#e5e7eb',
            lineColor: lastThemeWasLight ? '#1f2937' : '#e5e7eb',
            primaryColor: lastThemeWasLight ? '#eff6ff' : '#1e3a8a',
            primaryTextColor: lastThemeWasLight ? '#111827' : '#f9fafb',
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

    // Simplest way to make mermaid re-render the diagrams in the new theme is via refreshing the page

    for (const darkTheme of darkThemes) {
        document.getElementById(darkTheme).addEventListener('click', () => {
            if (lastThemeWasLight) {
                window.location.reload();
            }
        });
    }

    for (const lightTheme of lightThemes) {
        document.getElementById(lightTheme).addEventListener('click', () => {
            if (!lastThemeWasLight) {
                window.location.reload();
            }
        });
    }
})();
