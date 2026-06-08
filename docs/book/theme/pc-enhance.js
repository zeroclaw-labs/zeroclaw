/* ZeroClaw docs enhancement layer (Tier B PoC).
   - Right-hand "On this page" TOC built from content headings, with scroll-spy.
   - Hero banner injected on the landing page (introduction).
   - Reading-progress bar under the menu bar.
   No build-time coupling: everything is derived from the rendered DOM. */
(function () {
  'use strict';

  function ready(fn) {
    if (document.readyState !== 'loading') fn();
    else document.addEventListener('DOMContentLoaded', fn);
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

    const title = document.createElement('div');
    title.className = 'pc-toc-title';
    title.textContent = 'On this page';
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

    const hero = document.createElement('section');
    hero.className = 'pc-hero';
    // Static scaffold only — no interpolation of page-derived text here.
    hero.innerHTML =
      '<div class="pc-hero-glow"></div>' +
      '<div class="pc-hero-inner">' +
      '<div class="pc-hero-badge">ZeroClaw</div>' +
      '<h1 class="pc-hero-title"></h1>' +
      // Subtitle is intentionally hardcoded here: it is product positioning,
      // not page content, and changes rarely. If it needs to vary per build,
      // promote it to a data-hero-sub attribute read from the landing Markdown.
      '<p class="pc-hero-sub">Your personal AI assistant — one static binary, runs anywhere, no vendor lock-in.</p>' +
      '<div class="pc-hero-actions">' +
      '<a class="pc-btn pc-btn-primary" href="getting-started/quickstart.html">Quick start →</a>' +
      '<a class="pc-btn pc-btn-secondary" href="https://github.com/zeroclaw-labs/zeroclaw">GitHub</a>' +
      '</div></div>';
    // Insert the page-derived heading as text, never as HTML, so a crafted
    // heading cannot inject markup (textContent -> innerHTML re-encoding sink).
    hero.querySelector('.pc-hero-title').textContent = firstH1.textContent;
    firstH1.replaceWith(hero);
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

        const panel = document.createElement('div');
        panel.className = 'os-tab-panel';
        let node = h.nextElementSibling;
        while (node && node.tagName !== 'H3' && node.tagName !== 'H4') {
          const next = node.nextElementSibling;
          panel.appendChild(node);
          node = next;
        }
        panels.push(panel);
      });

      wrap.appendChild(labels);
      panels.forEach(function (p) {
        wrap.appendChild(p);
      });
      src.replaceWith(wrap);
    });
  }

  ready(function () {
    installProgressBar();
    installHero();
    installToc();
    wrapTables();
    installFoldableRows();
    installOsTabs();
  });
})();
