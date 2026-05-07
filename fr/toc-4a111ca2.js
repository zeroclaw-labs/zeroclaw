// Populate the sidebar
//
// This is a script, and not included directly in the page, to control the total size of the book.
// The TOC contains an entry for each page, so if each page includes a copy of the TOC,
// the total size of the page becomes O(n**2).
class MDBookSidebarScrollbox extends HTMLElement {
    constructor() {
        super();
    }
    connectedCallback() {
        this.innerHTML = '<ol class="chapter"><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="introduction.html"><strong aria-hidden="true">1.</strong> Introduction</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="philosophy.html"><strong aria-hidden="true">2.</strong> Philosophie</a></span></li><li class="chapter-item expanded "><li class="part-title">Premiers pas</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="getting-started/quick-start.html"><strong aria-hidden="true">3.</strong> Démarrage rapide</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="getting-started/yolo.html"><strong aria-hidden="true">4.</strong> Mode YOLO</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="getting-started/multi-model-setup.html"><strong aria-hidden="true">5.</strong> Configuration multi-modèles</a></span></li><li class="chapter-item expanded "><li class="part-title">Configuration</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="setup/linux.html"><strong aria-hidden="true">6.</strong> Linux</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="setup/macos.html"><strong aria-hidden="true">7.</strong> macOS</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="setup/windows.html"><strong aria-hidden="true">8.</strong> Windows</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="setup/container.html"><strong aria-hidden="true">9.</strong> Docker et conteneurs</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="setup/service.html"><strong aria-hidden="true">10.</strong> Gestion des services</a></span></li><li class="chapter-item expanded "><li class="part-title">Architecture</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="architecture/overview.html"><strong aria-hidden="true">11.</strong> Aperçu</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="architecture/request-lifecycle.html"><strong aria-hidden="true">12.</strong> Cycle de vie de la requête</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="architecture/crates.html"><strong aria-hidden="true">13.</strong> Crate</a></span></li><li class="chapter-item expanded "><li class="part-title">Référence</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="reference/cli.html"><strong aria-hidden="true">14.</strong> CLI</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="reference/config.html"><strong aria-hidden="true">15.</strong> Config</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="api.html"><strong aria-hidden="true">16.</strong> API (rustdoc)</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="gateway/api.html"><strong aria-hidden="true">17.</strong> Gateway HTTP API</a></span></li><li class="chapter-item expanded "><li class="part-title">Fournisseurs de modèles</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="providers/overview.html"><strong aria-hidden="true">18.</strong> Aperçu</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="providers/configuration.html"><strong aria-hidden="true">19.</strong> Configuration</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="providers/streaming.html"><strong aria-hidden="true">20.</strong> Diffusion en continu</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="providers/fallback-and-routing.html"><strong aria-hidden="true">21.</strong> Dépannage et routage</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="providers/catalog.html"><strong aria-hidden="true">22.</strong> Catalogue des fournisseurs</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="providers/custom.html"><strong aria-hidden="true">23.</strong> Fournisseurs personnalisés</a></span></li><li class="chapter-item expanded "><li class="part-title">Chaînes</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="channels/overview.html"><strong aria-hidden="true">24.</strong> Aperçu</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="channels/matrix.html"><strong aria-hidden="true">25.</strong> Matrice</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="channels/mattermost.html"><strong aria-hidden="true">26.</strong> Mattermost</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="channels/line.html"><strong aria-hidden="true">27.</strong> LINE</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="channels/nextcloud-talk.html"><strong aria-hidden="true">28.</strong> Nextcloud Talk</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="channels/chat-others.html"><strong aria-hidden="true">29.</strong> Autres plateformes de chat</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="channels/social.html"><strong aria-hidden="true">30.</strong> Social (Bluesky, Nostr, Twitter, Reddit)</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="channels/email.html"><strong aria-hidden="true">31.</strong> E-mail</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="channels/voice.html"><strong aria-hidden="true">32.</strong> Voix et téléphonie</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="channels/webhook.html"><strong aria-hidden="true">33.</strong> Webhooks</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="channels/acp.html"><strong aria-hidden="true">34.</strong> ACP (Agent Client Protocol)</a></span></li><li class="chapter-item expanded "><li class="part-title">Outils</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="tools/overview.html"><strong aria-hidden="true">35.</strong> Aperçu</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="tools/mcp.html"><strong aria-hidden="true">36.</strong> MCP (Protocole de Contexte du Modèle)</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="tools/browser.html"><strong aria-hidden="true">37.</strong> Automatisation du navigateur</a></span></li><li class="chapter-item expanded "><li class="part-title">Sécurité</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="security/overview.html"><strong aria-hidden="true">38.</strong> Aperçu</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="security/autonomy.html"><strong aria-hidden="true">39.</strong> Niveaux d&#39;autonomie</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="security/sandboxing.html"><strong aria-hidden="true">40.</strong> Isolation</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="security/tool-receipts.html"><strong aria-hidden="true">41.</strong> Reçus d&#39;outils</a></span></li><li class="chapter-item expanded "><li class="part-title">Opérations</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="ops/overview.html"><strong aria-hidden="true">42.</strong> Aperçu</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="ops/service.html"><strong aria-hidden="true">43.</strong> Service et démon</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="ops/observability.html"><strong aria-hidden="true">44.</strong> Journaux et observabilité</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="ops/troubleshooting.html"><strong aria-hidden="true">45.</strong> Dépannage</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="ops/network-deployment.html"><strong aria-hidden="true">46.</strong> Déploiement du réseau</a></span></li><li class="chapter-item expanded "><li class="part-title">Matériel</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hardware/index.html"><strong aria-hidden="true">47.</strong> Aperçu</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hardware/adding-boards-and-tools.html"><strong aria-hidden="true">48.</strong> Ajout de tableaux et d&#39;outils</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hardware/hardware-peripherals-design.html"><strong aria-hidden="true">49.</strong> Conception des périphériques</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hardware/arduino-uno-q-setup.html"><strong aria-hidden="true">50.</strong> Arduino Uno Q</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hardware/nucleo-setup.html"><strong aria-hidden="true">51.</strong> STM32 Nucleo</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hardware/android-setup.html"><strong aria-hidden="true">52.</strong> Android</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hardware/aardvark.html"><strong aria-hidden="true">53.</strong> Aardvark</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hardware/raspberry-pi-setup.html"><strong aria-hidden="true">54.</strong> Raspberry Pi</a></span></li><li class="chapter-item expanded "><li class="part-title">Procédures Opérationnelles Standard (SOP)</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="sop/index.html"><strong aria-hidden="true">55.</strong> Aperçu</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="sop/syntax.html"><strong aria-hidden="true">56.</strong> Syntaxe</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="sop/cookbook.html"><strong aria-hidden="true">57.</strong> Recettes</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="sop/connectivity.html"><strong aria-hidden="true">58.</strong> Connectivité</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="sop/observability.html"><strong aria-hidden="true">59.</strong> Observabilité</a></span></li><li class="chapter-item expanded "><li class="part-title">Développement</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="developing/plugin-protocol.html"><strong aria-hidden="true">60.</strong> Protocole du plugin</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="developing/extension-examples.html"><strong aria-hidden="true">61.</strong> Exemples d’extension</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="developing/building-docs.html"><strong aria-hidden="true">62.</strong> Génération de la documentation localement</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="developing/web.html"><strong aria-hidden="true">63.</strong> Building the web dashboard</a></span></li><li class="chapter-item expanded "><li class="part-title">Fondations</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="foundations/index.html"><strong aria-hidden="true">64.</strong> Aperçu</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="foundations/fnd-001-intentional-architecture.html"><strong aria-hidden="true">65.</strong> Architecture intentionnelle</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="foundations/fnd-002-documentation-standards.html"><strong aria-hidden="true">66.</strong> Normes de documentation</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="foundations/fnd-003-governance.html"><strong aria-hidden="true">67.</strong> Gouvernance</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="foundations/fnd-004-engineering-infrastructure.html"><strong aria-hidden="true">68.</strong> Infrastructure d&#39;ingénierie</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="foundations/fnd-005-contribution-culture.html"><strong aria-hidden="true">69.</strong> Culture de contribution</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="foundations/fnd-006-zero-compromise-in-practice.html"><strong aria-hidden="true">70.</strong> Zéro compromis en pratique</a></span></li><li class="chapter-item expanded "><li class="part-title">Contribuer</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="contributing/how-to.html"><strong aria-hidden="true">71.</strong> Comment contribuer</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="contributing/rfcs.html"><strong aria-hidden="true">72.</strong> Processus de la RFC</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="contributing/communication.html"><strong aria-hidden="true">73.</strong> Communication</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="contributing/privacy.html"><strong aria-hidden="true">74.</strong> Discipline relative à la confidentialité et aux données personnelles</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="contributing/testing.html"><strong aria-hidden="true">75.</strong> Tests</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="contributing/pr-review-protocol.html"><strong aria-hidden="true">76.</strong> Protocole de revue de code</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="contributing/cla.html"><strong aria-hidden="true">77.</strong> Accord de licence de contributeur</a></span></li><li class="chapter-item expanded "><li class="part-title">Mainteneurs</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="maintainers/index.html"><strong aria-hidden="true">78.</strong> Aperçu</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="maintainers/docs-and-translations.html"><strong aria-hidden="true">79.</strong> Docs et Traductions</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="maintainers/ci-and-actions.html"><strong aria-hidden="true">80.</strong> CI &amp; Actions</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="maintainers/skills.html"><strong aria-hidden="true">81.</strong> Compétences de Claude Code</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="maintainers/pr-workflow.html"><strong aria-hidden="true">82.</strong> Flux de travail PR</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="maintainers/reviewer-playbook.html"><strong aria-hidden="true">83.</strong> Guide du réviseur</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="maintainers/labels.html"><strong aria-hidden="true">84.</strong> Étiquettes</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="maintainers/superseding.html"><strong aria-hidden="true">85.</strong> PRs qui remplacent</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="maintainers/release-runbook.html"><strong aria-hidden="true">86.</strong> Livre de procédure de version</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="maintainers/changelog-generation.html"><strong aria-hidden="true">87.</strong> Génération du journal des modifications</a></span></li></ol>';
        // Set the current, active page, and reveal it if it's hidden
        let current_page = document.location.href.toString().split('#')[0].split('?')[0];
        if (current_page.endsWith('/')) {
            current_page += 'index.html';
        }
        const links = Array.prototype.slice.call(this.querySelectorAll('a'));
        const l = links.length;
        for (let i = 0; i < l; ++i) {
            const link = links[i];
            const href = link.getAttribute('href');
            if (href && !href.startsWith('#') && !/^(?:[a-z+]+:)?\/\//.test(href)) {
                link.href = path_to_root + href;
            }
            // The 'index' page is supposed to alias the first chapter in the book.
            if (link.href === current_page
                || i === 0
                && path_to_root === ''
                && current_page.endsWith('/index.html')) {
                link.classList.add('active');
                let parent = link.parentElement;
                while (parent) {
                    if (parent.tagName === 'LI' && parent.classList.contains('chapter-item')) {
                        parent.classList.add('expanded');
                    }
                    parent = parent.parentElement;
                }
            }
        }
        // Track and set sidebar scroll position
        this.addEventListener('click', e => {
            if (e.target.tagName === 'A') {
                sessionStorage.setItem('sidebar-scroll', this.scrollTop);
            }
        }, { passive: true });
        const sidebarScrollTop = sessionStorage.getItem('sidebar-scroll');
        sessionStorage.removeItem('sidebar-scroll');
        if (sidebarScrollTop) {
            // preserve sidebar scroll position when navigating via links within sidebar
            this.scrollTop = sidebarScrollTop;
        } else {
            // scroll sidebar to current active section when navigating via
            // 'next/previous chapter' buttons
            const activeSection = document.querySelector('#mdbook-sidebar .active');
            if (activeSection) {
                activeSection.scrollIntoView({ block: 'center' });
            }
        }
        // Toggle buttons
        const sidebarAnchorToggles = document.querySelectorAll('.chapter-fold-toggle');
        function toggleSection(ev) {
            ev.currentTarget.parentElement.parentElement.classList.toggle('expanded');
        }
        Array.from(sidebarAnchorToggles).forEach(el => {
            el.addEventListener('click', toggleSection);
        });
    }
}
window.customElements.define('mdbook-sidebar-scrollbox', MDBookSidebarScrollbox);


// ---------------------------------------------------------------------------
// Support for dynamically adding headers to the sidebar.

(function() {
    // This is used to detect which direction the page has scrolled since the
    // last scroll event.
    let lastKnownScrollPosition = 0;
    // This is the threshold in px from the top of the screen where it will
    // consider a header the "current" header when scrolling down.
    const defaultDownThreshold = 150;
    // Same as defaultDownThreshold, except when scrolling up.
    const defaultUpThreshold = 300;
    // The threshold is a virtual horizontal line on the screen where it
    // considers the "current" header to be above the line. The threshold is
    // modified dynamically to handle headers that are near the bottom of the
    // screen, and to slightly offset the behavior when scrolling up vs down.
    let threshold = defaultDownThreshold;
    // This is used to disable updates while scrolling. This is needed when
    // clicking the header in the sidebar, which triggers a scroll event. It
    // is somewhat finicky to detect when the scroll has finished, so this
    // uses a relatively dumb system of disabling scroll updates for a short
    // time after the click.
    let disableScroll = false;
    // Array of header elements on the page.
    let headers;
    // Array of li elements that are initially collapsed headers in the sidebar.
    // I'm not sure why eslint seems to have a false positive here.
    // eslint-disable-next-line prefer-const
    let headerToggles = [];
    // This is a debugging tool for the threshold which you can enable in the console.
    let thresholdDebug = false;

    // Updates the threshold based on the scroll position.
    function updateThreshold() {
        const scrollTop = window.pageYOffset || document.documentElement.scrollTop;
        const windowHeight = window.innerHeight;
        const documentHeight = document.documentElement.scrollHeight;

        // The number of pixels below the viewport, at most documentHeight.
        // This is used to push the threshold down to the bottom of the page
        // as the user scrolls towards the bottom.
        const pixelsBelow = Math.max(0, documentHeight - (scrollTop + windowHeight));
        // The number of pixels above the viewport, at least defaultDownThreshold.
        // Similar to pixelsBelow, this is used to push the threshold back towards
        // the top when reaching the top of the page.
        const pixelsAbove = Math.max(0, defaultDownThreshold - scrollTop);
        // How much the threshold should be offset once it gets close to the
        // bottom of the page.
        const bottomAdd = Math.max(0, windowHeight - pixelsBelow - defaultDownThreshold);
        let adjustedBottomAdd = bottomAdd;

        // Adjusts bottomAdd for a small document. The calculation above
        // assumes the document is at least twice the windowheight in size. If
        // it is less than that, then bottomAdd needs to be shrunk
        // proportional to the difference in size.
        if (documentHeight < windowHeight * 2) {
            const maxPixelsBelow = documentHeight - windowHeight;
            const t = 1 - pixelsBelow / Math.max(1, maxPixelsBelow);
            const clamp = Math.max(0, Math.min(1, t));
            adjustedBottomAdd *= clamp;
        }

        let scrollingDown = true;
        if (scrollTop < lastKnownScrollPosition) {
            scrollingDown = false;
        }

        if (scrollingDown) {
            // When scrolling down, move the threshold up towards the default
            // downwards threshold position. If near the bottom of the page,
            // adjustedBottomAdd will offset the threshold towards the bottom
            // of the page.
            const amountScrolledDown = scrollTop - lastKnownScrollPosition;
            const adjustedDefault = defaultDownThreshold + adjustedBottomAdd;
            threshold = Math.max(adjustedDefault, threshold - amountScrolledDown);
        } else {
            // When scrolling up, move the threshold down towards the default
            // upwards threshold position. If near the bottom of the page,
            // quickly transition the threshold back up where it normally
            // belongs.
            const amountScrolledUp = lastKnownScrollPosition - scrollTop;
            const adjustedDefault = defaultUpThreshold - pixelsAbove
                + Math.max(0, adjustedBottomAdd - defaultDownThreshold);
            threshold = Math.min(adjustedDefault, threshold + amountScrolledUp);
        }

        if (documentHeight <= windowHeight) {
            threshold = 0;
        }

        if (thresholdDebug) {
            const id = 'mdbook-threshold-debug-data';
            let data = document.getElementById(id);
            if (data === null) {
                data = document.createElement('div');
                data.id = id;
                data.style.cssText = `
                    position: fixed;
                    top: 50px;
                    right: 10px;
                    background-color: 0xeeeeee;
                    z-index: 9999;
                    pointer-events: none;
                `;
                document.body.appendChild(data);
            }
            data.innerHTML = `
                <table>
                  <tr><td>documentHeight</td><td>${documentHeight.toFixed(1)}</td></tr>
                  <tr><td>windowHeight</td><td>${windowHeight.toFixed(1)}</td></tr>
                  <tr><td>scrollTop</td><td>${scrollTop.toFixed(1)}</td></tr>
                  <tr><td>pixelsAbove</td><td>${pixelsAbove.toFixed(1)}</td></tr>
                  <tr><td>pixelsBelow</td><td>${pixelsBelow.toFixed(1)}</td></tr>
                  <tr><td>bottomAdd</td><td>${bottomAdd.toFixed(1)}</td></tr>
                  <tr><td>adjustedBottomAdd</td><td>${adjustedBottomAdd.toFixed(1)}</td></tr>
                  <tr><td>scrollingDown</td><td>${scrollingDown}</td></tr>
                  <tr><td>threshold</td><td>${threshold.toFixed(1)}</td></tr>
                </table>
            `;
            drawDebugLine();
        }

        lastKnownScrollPosition = scrollTop;
    }

    function drawDebugLine() {
        if (!document.body) {
            return;
        }
        const id = 'mdbook-threshold-debug-line';
        const existingLine = document.getElementById(id);
        if (existingLine) {
            existingLine.remove();
        }
        const line = document.createElement('div');
        line.id = id;
        line.style.cssText = `
            position: fixed;
            top: ${threshold}px;
            left: 0;
            width: 100vw;
            height: 2px;
            background-color: red;
            z-index: 9999;
            pointer-events: none;
        `;
        document.body.appendChild(line);
    }

    function mdbookEnableThresholdDebug() {
        thresholdDebug = true;
        updateThreshold();
        drawDebugLine();
    }

    window.mdbookEnableThresholdDebug = mdbookEnableThresholdDebug;

    // Updates which headers in the sidebar should be expanded. If the current
    // header is inside a collapsed group, then it, and all its parents should
    // be expanded.
    function updateHeaderExpanded(currentA) {
        // Add expanded to all header-item li ancestors.
        let current = currentA.parentElement;
        while (current) {
            if (current.tagName === 'LI' && current.classList.contains('header-item')) {
                current.classList.add('expanded');
            }
            current = current.parentElement;
        }
    }

    // Updates which header is marked as the "current" header in the sidebar.
    // This is done with a virtual Y threshold, where headers at or below
    // that line will be considered the current one.
    function updateCurrentHeader() {
        if (!headers || !headers.length) {
            return;
        }

        // Reset the classes, which will be rebuilt below.
        const els = document.getElementsByClassName('current-header');
        for (const el of els) {
            el.classList.remove('current-header');
        }
        for (const toggle of headerToggles) {
            toggle.classList.remove('expanded');
        }

        // Find the last header that is above the threshold.
        let lastHeader = null;
        for (const header of headers) {
            const rect = header.getBoundingClientRect();
            if (rect.top <= threshold) {
                lastHeader = header;
            } else {
                break;
            }
        }
        if (lastHeader === null) {
            lastHeader = headers[0];
            const rect = lastHeader.getBoundingClientRect();
            const windowHeight = window.innerHeight;
            if (rect.top >= windowHeight) {
                return;
            }
        }

        // Get the anchor in the summary.
        const href = '#' + lastHeader.id;
        const a = [...document.querySelectorAll('.header-in-summary')]
            .find(element => element.getAttribute('href') === href);
        if (!a) {
            return;
        }

        a.classList.add('current-header');

        updateHeaderExpanded(a);
    }

    // Updates which header is "current" based on the threshold line.
    function reloadCurrentHeader() {
        if (disableScroll) {
            return;
        }
        updateThreshold();
        updateCurrentHeader();
    }


    // When clicking on a header in the sidebar, this adjusts the threshold so
    // that it is located next to the header. This is so that header becomes
    // "current".
    function headerThresholdClick(event) {
        // See disableScroll description why this is done.
        disableScroll = true;
        setTimeout(() => {
            disableScroll = false;
        }, 100);
        // requestAnimationFrame is used to delay the update of the "current"
        // header until after the scroll is done, and the header is in the new
        // position.
        requestAnimationFrame(() => {
            requestAnimationFrame(() => {
                // Closest is needed because if it has child elements like <code>.
                const a = event.target.closest('a');
                const href = a.getAttribute('href');
                const targetId = href.substring(1);
                const targetElement = document.getElementById(targetId);
                if (targetElement) {
                    threshold = targetElement.getBoundingClientRect().bottom;
                    updateCurrentHeader();
                }
            });
        });
    }

    // Takes the nodes from the given head and copies them over to the
    // destination, along with some filtering.
    function filterHeader(source, dest) {
        const clone = source.cloneNode(true);
        clone.querySelectorAll('mark').forEach(mark => {
            mark.replaceWith(...mark.childNodes);
        });
        dest.append(...clone.childNodes);
    }

    // Scans page for headers and adds them to the sidebar.
    document.addEventListener('DOMContentLoaded', function() {
        const activeSection = document.querySelector('#mdbook-sidebar .active');
        if (activeSection === null) {
            return;
        }

        const main = document.getElementsByTagName('main')[0];
        headers = Array.from(main.querySelectorAll('h2, h3, h4, h5, h6'))
            .filter(h => h.id !== '' && h.children.length && h.children[0].tagName === 'A');

        if (headers.length === 0) {
            return;
        }

        // Build a tree of headers in the sidebar.

        const stack = [];

        const firstLevel = parseInt(headers[0].tagName.charAt(1));
        for (let i = 1; i < firstLevel; i++) {
            const ol = document.createElement('ol');
            ol.classList.add('section');
            if (stack.length > 0) {
                stack[stack.length - 1].ol.appendChild(ol);
            }
            stack.push({level: i + 1, ol: ol});
        }

        // The level where it will start folding deeply nested headers.
        const foldLevel = 3;

        for (let i = 0; i < headers.length; i++) {
            const header = headers[i];
            const level = parseInt(header.tagName.charAt(1));

            const currentLevel = stack[stack.length - 1].level;
            if (level > currentLevel) {
                // Begin nesting to this level.
                for (let nextLevel = currentLevel + 1; nextLevel <= level; nextLevel++) {
                    const ol = document.createElement('ol');
                    ol.classList.add('section');
                    const last = stack[stack.length - 1];
                    const lastChild = last.ol.lastChild;
                    // Handle the case where jumping more than one nesting
                    // level, which doesn't have a list item to place this new
                    // list inside of.
                    if (lastChild) {
                        lastChild.appendChild(ol);
                    } else {
                        last.ol.appendChild(ol);
                    }
                    stack.push({level: nextLevel, ol: ol});
                }
            } else if (level < currentLevel) {
                while (stack.length > 1 && stack[stack.length - 1].level >= level) {
                    stack.pop();
                }
            }

            const li = document.createElement('li');
            li.classList.add('header-item');
            li.classList.add('expanded');
            if (level < foldLevel) {
                li.classList.add('expanded');
            }
            const span = document.createElement('span');
            span.classList.add('chapter-link-wrapper');
            const a = document.createElement('a');
            span.appendChild(a);
            a.href = '#' + header.id;
            a.classList.add('header-in-summary');
            filterHeader(header.children[0], a);
            a.addEventListener('click', headerThresholdClick);
            const nextHeader = headers[i + 1];
            if (nextHeader !== undefined) {
                const nextLevel = parseInt(nextHeader.tagName.charAt(1));
                if (nextLevel > level && level >= foldLevel) {
                    const toggle = document.createElement('a');
                    toggle.classList.add('chapter-fold-toggle');
                    toggle.classList.add('header-toggle');
                    toggle.addEventListener('click', () => {
                        li.classList.toggle('expanded');
                    });
                    const toggleDiv = document.createElement('div');
                    toggleDiv.textContent = '❱';
                    toggle.appendChild(toggleDiv);
                    span.appendChild(toggle);
                    headerToggles.push(li);
                }
            }
            li.appendChild(span);

            const currentParent = stack[stack.length - 1];
            currentParent.ol.appendChild(li);
        }

        const onThisPage = document.createElement('div');
        onThisPage.classList.add('on-this-page');
        onThisPage.append(stack[0].ol);
        const activeItemSpan = activeSection.parentElement;
        activeItemSpan.after(onThisPage);
    });

    document.addEventListener('DOMContentLoaded', reloadCurrentHeader);
    document.addEventListener('scroll', reloadCurrentHeader, { passive: true });
})();

