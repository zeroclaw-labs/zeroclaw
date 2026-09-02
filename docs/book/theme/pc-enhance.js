/* ZeroClaw docs enhancement layer (Tier B PoC).
   - Right-hand page TOC built from content headings, with scroll-spy.
   - Persistent reader text scaling and a narrow-screen TOC toggle.
   - Keyboard/pointer Mermaid diagram expansion.
   - Hero banner injected on the landing page (introduction).
   - Reading-progress bar under the menu bar.
   No build-time coupling: everything is derived from the rendered DOM. */
(function () {
  'use strict';

  const READER_SCALE_KEY = 'pc-reader-font-scale';
  const READER_SCALE_MIN = 0.85;
  const READER_SCALE_MAX = 1.4;
  const READER_SCALE_STEP = 0.1;

  const LOCALE_TEXT = {
    en: {
      onThisPage: 'On this page',
      quickStart: 'Quickstart',
      readerControls: 'Reading settings',
      decreaseText: 'Decrease text size',
      increaseText: 'Increase text size',
      resetText: 'Reset text size',
      textSize: 'Text size',
      showToc: 'Show page contents',
      hideToc: 'Hide page contents',
      expandDiagram: 'Open diagram zoom',
      closeDiagram: 'Close diagram',
      zoomIn: 'Zoom in',
      zoomOut: 'Zoom out',
      resetZoom: 'Reset zoom',
      zoomLevel: 'Zoom level',
    },
    es: {
      onThisPage: 'En esta página',
      quickStart: 'Inicio rápido',
      readerControls: 'Ajustes de lectura',
      decreaseText: 'Reducir tamaño del texto',
      increaseText: 'Aumentar tamaño del texto',
      resetText: 'Restablecer tamaño del texto',
      textSize: 'Tamaño del texto',
      showToc: 'Mostrar contenido de la página',
      hideToc: 'Ocultar contenido de la página',
      expandDiagram: 'Abrir zoom del diagrama',
      closeDiagram: 'Cerrar diagrama',
      zoomIn: 'Ampliar',
      zoomOut: 'Reducir',
      resetZoom: 'Restablecer zoom',
      zoomLevel: 'Nivel de zoom',
    },
    fr: {
      onThisPage: 'Sur cette page',
      quickStart: 'Démarrage rapide',
      readerControls: 'Réglages de lecture',
      decreaseText: 'Réduire la taille du texte',
      increaseText: 'Augmenter la taille du texte',
      resetText: 'Réinitialiser la taille du texte',
      textSize: 'Taille du texte',
      showToc: 'Afficher le contenu de la page',
      hideToc: 'Masquer le contenu de la page',
      expandDiagram: 'Ouvrir le zoom du diagramme',
      closeDiagram: 'Fermer le diagramme',
      zoomIn: 'Zoom avant',
      zoomOut: 'Zoom arrière',
      resetZoom: 'Réinitialiser le zoom',
      zoomLevel: 'Niveau de zoom',
    },
    ja: {
      onThisPage: 'このページ',
      quickStart: 'クイックスタート',
      readerControls: '閲覧設定',
      decreaseText: '文字を小さくする',
      increaseText: '文字を大きくする',
      resetText: '文字サイズをリセット',
      textSize: '文字サイズ',
      showToc: 'ページ目次を表示',
      hideToc: 'ページ目次を非表示',
      expandDiagram: '図を拡大して開く',
      closeDiagram: '図を閉じる',
      zoomIn: '拡大',
      zoomOut: '縮小',
      resetZoom: 'ズームをリセット',
      zoomLevel: 'ズーム倍率',
    },
    'zh-CN': {
      onThisPage: '本页目录',
      quickStart: '快速入门',
      readerControls: '阅读设置',
      decreaseText: '缩小字号',
      increaseText: '放大字号',
      resetText: '重置字号',
      textSize: '字号',
      showToc: '显示本页目录',
      hideToc: '隐藏本页目录',
      expandDiagram: '打开图表缩放',
      closeDiagram: '关闭图表',
      zoomIn: '放大',
      zoomOut: '缩小',
      resetZoom: '重置缩放',
      zoomLevel: '缩放比例',
    },
  };

  function localeText(key, fallback) {
    const lang = document.documentElement.lang || 'en';
    const exact = LOCALE_TEXT[lang];
    const base = LOCALE_TEXT[lang.split('-')[0]];
    return (exact && exact[key]) || (base && base[key]) || fallback;
  }

  function ready(fn) {
    if (document.readyState !== 'loading') fn();
    else document.addEventListener('DOMContentLoaded', fn);
  }

  function clamp(value, min, max) {
    return Math.min(max, Math.max(min, value));
  }

  function readReaderScale() {
    let value = 1;
    try {
      value = Number.parseFloat(localStorage.getItem(READER_SCALE_KEY));
    } catch (e) {
      // Storage can be unavailable in private or embedded browser contexts.
    }
    if (!Number.isFinite(value)) value = 1;
    return clamp(Math.round(value * 10) / 10, READER_SCALE_MIN, READER_SCALE_MAX);
  }

  function saveReaderScale(value) {
    try {
      localStorage.setItem(READER_SCALE_KEY, String(value));
    } catch (e) {
      // The control still works for this page when persistence is unavailable.
    }
  }

  function formatPercent(value) {
    return Math.round(value * 100) + '%';
  }

  // ── Persistent text scaling ───────────────────────────────────────────
  function installReaderControls() {
    const buttons = document.querySelector('#mdbook-menu-bar .left-buttons');
    if (!buttons || document.getElementById('pc-reader-controls')) return;

    let scale = readReaderScale();
    const wrapper = document.createElement('div');
    wrapper.id = 'pc-reader-controls';
    wrapper.className = 'pc-reader-controls';

    const toggle = document.createElement('button');
    toggle.type = 'button';
    toggle.className = 'icon-button pc-reader-toggle';
    toggle.textContent = 'Aa';
    toggle.title = localeText('readerControls', 'Reading settings');
    toggle.setAttribute('aria-label', toggle.title);
    toggle.setAttribute('aria-haspopup', 'dialog');
    toggle.setAttribute('aria-expanded', 'false');
    toggle.setAttribute('aria-controls', 'pc-reader-popup');

    const popup = document.createElement('div');
    popup.id = 'pc-reader-popup';
    popup.className = 'pc-reader-popup';
    popup.hidden = true;
    popup.setAttribute('role', 'dialog');
    popup.setAttribute('aria-labelledby', 'pc-reader-popup-title');

    const title = document.createElement('div');
    title.id = 'pc-reader-popup-title';
    title.className = 'pc-reader-popup-title';
    title.textContent = localeText('readerControls', 'Reading settings');

    const label = document.createElement('div');
    label.className = 'pc-reader-scale-label';
    label.textContent = localeText('textSize', 'Text size');

    const row = document.createElement('div');
    row.className = 'pc-reader-scale-row';

    const decrease = document.createElement('button');
    decrease.type = 'button';
    decrease.className = 'pc-reader-scale-button';
    decrease.textContent = 'A−';
    decrease.title = localeText('decreaseText', 'Decrease text size');
    decrease.setAttribute('aria-label', decrease.title);

    const value = document.createElement('output');
    value.className = 'pc-reader-scale-value';
    value.setAttribute('aria-live', 'polite');

    const increase = document.createElement('button');
    increase.type = 'button';
    increase.className = 'pc-reader-scale-button';
    increase.textContent = 'A+';
    increase.title = localeText('increaseText', 'Increase text size');
    increase.setAttribute('aria-label', increase.title);

    const reset = document.createElement('button');
    reset.type = 'button';
    reset.className = 'pc-reader-reset-button';
    reset.textContent = localeText('resetText', 'Reset text size');
    reset.title = reset.textContent;

    function applyScale(next) {
      scale = clamp(Math.round(next * 10) / 10, READER_SCALE_MIN, READER_SCALE_MAX);
      document.documentElement.style.setProperty('--pc-reader-scale', String(scale));
      value.textContent = formatPercent(scale);
      decrease.disabled = scale <= READER_SCALE_MIN;
      increase.disabled = scale >= READER_SCALE_MAX;
      saveReaderScale(scale);
    }

    function setOpen(open) {
      popup.hidden = !open;
      toggle.setAttribute('aria-expanded', String(open));
      if (open) decrease.focus();
    }

    decrease.addEventListener('click', function () {
      applyScale(scale - READER_SCALE_STEP);
    });
    increase.addEventListener('click', function () {
      applyScale(scale + READER_SCALE_STEP);
    });
    reset.addEventListener('click', function () {
      applyScale(1);
    });
    toggle.addEventListener('click', function (e) {
      e.stopPropagation();
      setOpen(popup.hidden);
    });
    popup.addEventListener('click', function (e) {
      e.stopPropagation();
    });
    document.addEventListener('click', function () {
      if (!popup.hidden) setOpen(false);
    });
    document.addEventListener('keydown', function (e) {
      if (e.key === 'Escape' && !popup.hidden) {
        setOpen(false);
        toggle.focus();
      }
    });

    row.append(decrease, value, increase);
    popup.append(title, label, row, reset);
    wrapper.append(toggle, popup);
    buttons.appendChild(wrapper);
    applyScale(scale);
  }

  // ── Reading progress bar ───────────────────────────────────────────────
  function installProgressBar() {
    const bar = document.createElement('div');
    bar.id = 'pc-progress';
    document.body.appendChild(bar);
    const content = document.getElementById('mdbook-content');
    function update() {
      const scroller = document.documentElement;
      const max = scroller.scrollHeight - scroller.clientHeight;
      const pct = max > 0 ? (scroller.scrollTop / max) * 100 : 0;
      bar.style.width = pct + '%';
    }
    window.addEventListener('scroll', update, { passive: true });
    window.addEventListener('resize', update, { passive: true });
    update();
    void content;
  }

  // ── Right-hand TOC + scroll-spy ────────────────────────────────────────
  function installToc() {
    const toc = document.getElementById('pc-page-toc');
    const main = document.querySelector('#mdbook-content main');
    if (!toc || !main) return;

    const headings = Array.from(main.querySelectorAll('h2, h3')).filter(
      (h) => h.id,
    );
    if (headings.length < 2) {
      toc.remove();
      document.getElementById('mdbook-content')?.classList.add('pc-no-toc');
      return;
    }

    const tocToggle = document.createElement('button');
    tocToggle.type = 'button';
    tocToggle.className = 'pc-toc-toggle';
    tocToggle.setAttribute('aria-controls', 'pc-page-toc');
    tocToggle.setAttribute('aria-expanded', 'false');
    tocToggle.textContent = localeText('showToc', 'Show page contents');
    tocToggle.addEventListener('click', function () {
      const open = !toc.classList.contains('pc-toc-open');
      toc.classList.toggle('pc-toc-open', open);
      tocToggle.setAttribute('aria-expanded', String(open));
      tocToggle.textContent = localeText(open ? 'hideToc' : 'showToc', open ? 'Hide page contents' : 'Show page contents');
    });
    toc.parentNode.insertBefore(tocToggle, toc);

    const title = document.createElement('div');
    title.className = 'pc-toc-title';
    title.textContent = localeText('onThisPage', 'On this page');
    toc.setAttribute('aria-label', title.textContent);
    toc.appendChild(title);

    const list = document.createElement('ul');
    list.className = 'pc-toc-list';
    const links = [];
    for (const h of headings) {
      const li = document.createElement('li');
      li.className = 'pc-toc-item pc-toc-' + h.tagName.toLowerCase();
      const a = document.createElement('a');
      a.href = '#' + h.id;
      a.textContent = h.textContent.replace(/\u00B6/g, '').trim();
      a.addEventListener('click', function (e) {
        e.preventDefault();
        h.scrollIntoView({ behavior: 'smooth', block: 'start' });
        history.replaceState(null, '', '#' + h.id);
        toc.classList.remove('pc-toc-open');
        tocToggle.setAttribute('aria-expanded', 'false');
        tocToggle.textContent = localeText('showToc', 'Show page contents');
      });
      li.appendChild(a);
      list.appendChild(li);
      links.push({ a: a, h: h });
    }
    toc.appendChild(list);

    const byId = new Map(links.map((l) => [l.h.id, l.a]));
    let active = null;
    const spy = new IntersectionObserver(
      function (entries) {
        for (const entry of entries) {
          if (entry.isIntersecting) {
            const a = byId.get(entry.target.id);
            if (a && a !== active) {
              if (active) active.classList.remove('pc-toc-active');
              a.classList.add('pc-toc-active');
              active = a;
            }
          }
        }
      },
      { rootMargin: '0px 0px -75% 0px', threshold: 0 },
    );
    headings.forEach((h) => spy.observe(h));
  }

  // ── Mermaid diagram expansion ─────────────────────────────────────────
  function installDiagramZoom() {
    const main = document.querySelector('#mdbook-content main');
    if (!main || document.getElementById('pc-diagram-modal')) return;

    let activeTrigger = null;
    let activeDiagram = null;
    let zoom = 1;
    let panX = 0;
    let panY = 0;
    let dragging = false;
    let dragStartX = 0;
    let dragStartY = 0;

    const modal = document.createElement('div');
    modal.id = 'pc-diagram-modal';
    modal.className = 'pc-diagram-modal';
    modal.hidden = true;
    modal.setAttribute('role', 'dialog');
    modal.setAttribute('aria-modal', 'true');
    modal.setAttribute('aria-labelledby', 'pc-diagram-modal-title');

    const surface = document.createElement('div');
    surface.className = 'pc-diagram-surface';

    const heading = document.createElement('h2');
    heading.id = 'pc-diagram-modal-title';
    heading.className = 'pc-diagram-modal-title';
    heading.textContent = localeText('expandDiagram', 'Open diagram zoom');

    const close = document.createElement('button');
    close.type = 'button';
    close.className = 'pc-diagram-close';
    close.textContent = '×';
    close.title = localeText('closeDiagram', 'Close diagram');
    close.setAttribute('aria-label', close.title);

    const stage = document.createElement('div');
    stage.className = 'pc-diagram-stage';
    stage.setAttribute('role', 'region');
    stage.setAttribute('aria-label', localeText('expandDiagram', 'Open diagram zoom'));

    const controls = document.createElement('div');
    controls.className = 'pc-diagram-controls';

    function controlButton(label, text) {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'pc-diagram-control';
      button.title = label;
      button.setAttribute('aria-label', label);
      button.textContent = text;
      return button;
    }

    const zoomOut = controlButton(localeText('zoomOut', 'Zoom out'), '−');
    const zoomValue = document.createElement('output');
    zoomValue.className = 'pc-diagram-zoom-value';
    zoomValue.setAttribute('aria-live', 'polite');
    const zoomIn = controlButton(localeText('zoomIn', 'Zoom in'), '+');
    const zoomReset = controlButton(localeText('resetZoom', 'Reset zoom'), '1:1');
    zoomReset.classList.add('pc-diagram-reset');

    function diagramSvg() {
      return stage.querySelector('svg');
    }

    function applyTransform() {
      const svg = diagramSvg();
      if (!svg) return;
      svg.style.transform = 'translate(' + panX + 'px, ' + panY + 'px) scale(' + zoom + ')';
      zoomValue.textContent = formatPercent(zoom);
      zoomOut.disabled = zoom <= 1;
      zoomIn.disabled = zoom >= 4;
    }

    function setZoom(next) {
      const previous = zoom;
      zoom = clamp(Math.round(next * 4) / 4, 1, 4);
      if (zoom === 1) {
        panX = 0;
        panY = 0;
      } else if (previous !== zoom) {
        panX = clamp(panX, -800, 800);
        panY = clamp(panY, -800, 800);
      }
      applyTransform();
    }

    function closeModal() {
      modal.hidden = true;
      document.documentElement.classList.remove('pc-diagram-modal-open');

      if (activeDiagram) {
        const {
          svg,
          placeholder,
          originalTransform,
          originalAriaHidden,
          originalTabIndex,
        } = activeDiagram;
        svg.style.transform = originalTransform;
        if (originalAriaHidden === null) svg.removeAttribute('aria-hidden');
        else svg.setAttribute('aria-hidden', originalAriaHidden);
        if (originalTabIndex === null) svg.removeAttribute('tabindex');
        else svg.setAttribute('tabindex', originalTabIndex);
        if (placeholder.isConnected) placeholder.replaceWith(svg);
        else stage.replaceChildren();
        activeDiagram = null;
      }

      stage.replaceChildren();
      if (activeTrigger) activeTrigger.focus();
      activeTrigger = null;
    }

    function openModal(svg, trigger) {
      activeTrigger = trigger;
      zoom = 1;
      panX = 0;
      panY = 0;
      // Keep Mermaid's original SVG node so its generated IDs remain unique.
      // A sized placeholder preserves the page layout while the node is in the
      // dialog, then the same node is restored when the dialog closes.
      const placeholder = document.createElement('span');
      placeholder.className = 'pc-diagram-placeholder';
      placeholder.setAttribute('aria-hidden', 'true');
      const rect = svg.getBoundingClientRect();
      const computed = window.getComputedStyle(svg);
      placeholder.style.display = computed.display === 'inline' ? 'inline-block' : computed.display;
      placeholder.style.width = rect.width + 'px';
      placeholder.style.height = rect.height + 'px';
      placeholder.style.margin = computed.margin;
      placeholder.style.verticalAlign = computed.verticalAlign;
      activeDiagram = {
        svg: svg,
        placeholder: placeholder,
        originalTransform: svg.style.transform,
        originalAriaHidden: svg.getAttribute('aria-hidden'),
        originalTabIndex: svg.getAttribute('tabindex'),
      };
      svg.replaceWith(placeholder);
      svg.setAttribute('aria-hidden', 'true');
      svg.removeAttribute('tabindex');
      stage.replaceChildren(svg);
      modal.hidden = false;
      document.documentElement.classList.add('pc-diagram-modal-open');
      applyTransform();
      close.focus();
    }

    close.addEventListener('click', closeModal);
    modal.addEventListener('click', function (e) {
      if (e.target === modal) closeModal();
    });
    zoomOut.addEventListener('click', function () { setZoom(zoom - 0.25); });
    zoomIn.addEventListener('click', function () { setZoom(zoom + 0.25); });
    zoomReset.addEventListener('click', function () { setZoom(1); });
    stage.addEventListener('wheel', function (e) {
      if (modal.hidden) return;
      e.preventDefault();
      setZoom(zoom + (e.deltaY < 0 ? 0.25 : -0.25));
    }, { passive: false });
    stage.addEventListener('pointerdown', function (e) {
      if (zoom <= 1) return;
      dragging = true;
      dragStartX = e.clientX - panX;
      dragStartY = e.clientY - panY;
      stage.setPointerCapture(e.pointerId);
      stage.classList.add('pc-diagram-dragging');
    });
    stage.addEventListener('pointermove', function (e) {
      if (!dragging) return;
      panX = clamp(e.clientX - dragStartX, -1200, 1200);
      panY = clamp(e.clientY - dragStartY, -1200, 1200);
      applyTransform();
    });
    function stopDragging(e) {
      dragging = false;
      if (e && stage.hasPointerCapture(e.pointerId)) stage.releasePointerCapture(e.pointerId);
      stage.classList.remove('pc-diagram-dragging');
    }
    stage.addEventListener('pointerup', stopDragging);
    stage.addEventListener('pointercancel', stopDragging);
    document.addEventListener('keydown', function (e) {
      if (!modal.hidden && e.key === 'Escape') closeModal();
    });

    controls.append(zoomOut, zoomValue, zoomIn, zoomReset);
    surface.append(heading, close, stage, controls);
    modal.appendChild(surface);
    document.body.appendChild(modal);

    function wire() {
      main.querySelectorAll('.mermaid svg, pre.mermaid svg').forEach(function (svg) {
        const host = svg.closest('.mermaid, pre.mermaid') || svg;
        if (host.dataset.pcDiagramZoomWired) return;
        host.dataset.pcDiagramZoomWired = '1';
        host.classList.add('pc-diagram-zoomable');
        host.tabIndex = 0;
        host.setAttribute('role', 'button');
        host.setAttribute('aria-label', localeText('expandDiagram', 'Open diagram zoom'));
        host.setAttribute('aria-keyshortcuts', 'Enter Space');
        // Mermaid can replace the rendered SVG while the page is settling.
        // Resolve the current child at activation time so the dialog moves the
        // live node rather than a stale render captured during wiring.
        function openCurrentDiagram() {
          const currentSvg =
            host.matches('svg') ? host : host.querySelector('svg');
          if (currentSvg) openModal(currentSvg, host);
        }
        host.addEventListener('click', function (e) {
          if (e.target.closest('a')) return;
          openCurrentDiagram();
        });
        host.addEventListener('keydown', function (e) {
          if (e.key !== 'Enter' && e.key !== ' ') return;
          e.preventDefault();
          openCurrentDiagram();
        });
      });
    }

    wire();
    const observer = new MutationObserver(wire);
    observer.observe(main, { childList: true, subtree: true });
    window.setTimeout(wire, 120);
  }

  // ── Hero banner on the landing page ────────────────────────────────────
  function installHero() {
    const main = document.querySelector('#mdbook-content main');
    if (!main) return;
    const path = window.location.pathname;
    const isLanding = /\/(index|introduction)\.html$/.test(path) || /\/[a-zA-Z-]+\/$/.test(path);
    if (!isLanding) return;
    if (main.querySelector('.pc-hero')) return;

    const firstH1 = main.querySelector('h1');
    if (!firstH1) return;
    // Only treat as the true landing page when the first heading is the intro.
    const t = firstH1.textContent.toLowerCase();
    if (!/introduction|zeroclaw|welcome|overview/.test(t)) return;

    const intro = firstH1.nextElementSibling?.matches('p')
      ? firstH1.nextElementSibling
      : null;
    const subtitle =
      intro?.textContent.trim() || 'Personal AI assistant you own, written in Rust.';
    const quickstart = Array.from(main.querySelectorAll('a[href]')).find((a) => {
      const href = a.getAttribute('href') || '';
      return /(^|\/)getting-started\/quick-?start\.html$/.test(href);
    });
    const quickstartHref =
      quickstart?.getAttribute('href') || 'getting-started/quickstart.html';
    const quickstartText =
      localeText('quickStart', quickstart?.textContent.trim() || 'Quickstart');

    const hero = document.createElement('section');
    hero.className = 'pc-hero';
    hero.innerHTML =
      '<div class="pc-hero-glow"></div>' +
      '<div class="pc-hero-inner">' +
      '<div class="pc-hero-badge">ZeroClaw</div>' +
      '<h1 class="pc-hero-title"></h1>' +
      '<p class="pc-hero-sub"></p>' +
      '<div class="pc-hero-actions">' +
      '<a class="pc-btn pc-btn-primary"></a>' +
      '<a class="pc-btn pc-btn-secondary" href="https://github.com/zeroclaw-labs/zeroclaw">GitHub</a>' +
      '</div></div>';
    // Insert the page-derived heading as text, never as HTML, so a crafted
    // heading or translation cannot inject markup.
    hero.querySelector('.pc-hero-title').textContent = firstH1.textContent;
    hero.querySelector('.pc-hero-sub').textContent = subtitle;
    const primary = hero.querySelector('.pc-btn-primary');
    primary.href = quickstartHref;
    primary.textContent = quickstartText.replace(/\s*→\s*$/, '') + ' →';
    firstH1.replaceWith(hero);
    if (intro) intro.remove();
  }

  // ── Wrap tables for horizontal scroll on narrow screens ────────────────
  function wrapTables() {
    const main = document.querySelector('#mdbook-content main');
    if (!main) return;
    main.querySelectorAll('table').forEach(function (tbl) {
      if (tbl.parentElement.classList.contains('pc-table-wrap')) return;
      const wrap = document.createElement('div');
      wrap.className = 'pc-table-wrap';
      tbl.replaceWith(wrap);
      wrap.appendChild(tbl);
    });
  }

  // ── Make foldable section rows fully clickable ─────────────────────────
  // mdBook only binds the fold toggle to the small `❱` arrow. Widen the hit
  // target to the entire parent row by forwarding row clicks to the toggle.
  // The sidebar is rendered asynchronously by toc.js, so we wait for it.
  function installFoldableRows() {
    function wire(scope) {
      const wrappers = scope.querySelectorAll('.chapter-link-wrapper');
      wrappers.forEach(function (wrap) {
        const toggle = wrap.querySelector(':scope > a.chapter-fold-toggle');
        if (!toggle || wrap.dataset.pcFoldWired) return;
        // Only parent rows that are label-only (no real link) should toggle
        // on full-row click; rows that are also links keep their navigation.
        const link = wrap.querySelector(':scope > a[href]');
        wrap.dataset.pcFoldWired = '1';
        wrap.classList.add('pc-foldable-row');
        wrap.addEventListener('click', function (e) {
          if (e.target.closest('a.chapter-fold-toggle')) return; // native path
          if (link && e.target.closest('a[href]') === link) return; // real link
          e.preventDefault();
          toggle.click();
        });
      });
    }

    const sidebar = document.getElementById('mdbook-sidebar');
    if (!sidebar) return;
    wire(sidebar);
    // toc.js may populate/replace the scrollbox after load; observe for it.
    const box = sidebar.querySelector('.sidebar-scrollbox') || sidebar;
    const obs = new MutationObserver(function () {
      wire(sidebar);
    });
    obs.observe(box, { childList: true, subtree: true });
  }

  // ── OS tabs ────────────────────────────────────────────────────────────
  // Authoring: wrap the divergent content in a single
  //   <div class="os-tabs-src"> ... </div>
  // with one H3/H4 heading per OS (Linux / macOS / Windows). Each heading and
  // the markdown beneath it (labelled fenced blocks, prose) becomes a tab
  // panel. This transform replaces the source div with the radio/label/panel
  // widget, generating unique ids per instance so multiple pickers coexist.
  let osTabsSeq = 0;
  function installOsTabs() {
    const sources = document.querySelectorAll('.os-tabs-src');
    sources.forEach(function (src) {
      const headings = Array.from(src.children).filter(function (el) {
        return el.tagName === 'H3' || el.tagName === 'H4';
      });
      if (headings.length < 1) return;

      const group = 'os-tabs-' + ++osTabsSeq;
      const wrap = document.createElement('div');
      wrap.className = 'os-tabs';

      const labels = document.createElement('nav');
      labels.className = 'os-tab-labels';

      const panels = [];
      const labelEls = [];
      headings.forEach(function (h, i) {
        const id = group + '-' + i;
        const radio = document.createElement('input');
        radio.type = 'radio';
        radio.name = group;
        radio.id = id;
        if (i === 0) radio.checked = true;
        wrap.appendChild(radio);

        const label = document.createElement('label');
        label.setAttribute('for', id);
        label.textContent = h.textContent.replace(/\u00B6/g, '').trim();
        labels.appendChild(label);
        labelEls.push(label);

        const panel = document.createElement('div');
        panel.className = 'os-tab-panel';
        let node = h.nextElementSibling;
        while (node && node.tagName !== 'H3' && node.tagName !== 'H4') {
          const next = node.nextElementSibling;
          panel.appendChild(node);
          node = next;
        }
        panels.push(panel);

        // Active-state is driven here (any number of tabs), not by positional
        // CSS selectors, so adding a tab needs no CSS change.
        radio.addEventListener('change', function () {
          panels.forEach(function (p, j) {
            p.classList.toggle('is-active', j === i);
          });
          labelEls.forEach(function (l, j) {
            l.classList.toggle('is-active', j === i);
          });
        });
        if (i === 0) {
          panel.classList.add('is-active');
          label.classList.add('is-active');
        }
      });

      wrap.appendChild(labels);
      panels.forEach(function (p) {
        wrap.appendChild(p);
      });
      src.replaceWith(wrap);
    });
  }

  ready(function () {
    installReaderControls();
    installProgressBar();
    installHero();
    installToc();
    wrapTables();
    installFoldableRows();
    installOsTabs();
    installDiagramZoom();
  });
})();
