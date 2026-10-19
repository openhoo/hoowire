/* Teamwire GIF picker — local desktop extension. */
(() => {
  'use strict';
  const translations = {
    en: {
      open: 'Search GIFs', title: 'GIFs', search: 'Search GIPHY', close: 'Close GIF picker',
      hint: 'Select a GIF to add it to your message', loading: 'Searching for GIFs…',
      adding: 'Adding GIF…', empty: 'No GIFs found. Try another search.',
      error: 'Could not load GIFs. Please try again.', retry: 'Try again',
      results: 'GIFs found', by: 'by', attribution: 'Powered by GIPHY',
      categories: ['Reactions', 'Happy', 'Thanks', 'Yes', 'No', 'Celebrate', 'Facepalm'],
    },
    de: {
      open: 'GIFs suchen', title: 'GIFs', search: 'GIPHY durchsuchen', close: 'GIF-Auswahl schließen',
      hint: 'Wähle ein GIF aus, um es zur Nachricht hinzuzufügen', loading: 'GIFs werden gesucht…',
      adding: 'GIF wird hinzugefügt…', empty: 'Keine GIFs gefunden. Probiere einen anderen Suchbegriff.',
      error: 'GIFs konnten nicht geladen werden. Bitte erneut versuchen.', retry: 'Erneut versuchen',
      results: 'GIFs gefunden', by: 'von', attribution: 'Powered by GIPHY',
      categories: ['Reaktionen', 'Glücklich', 'Danke', 'Ja', 'Nein', 'Feiern', 'Facepalm'],
    },
  };
  let active = null;
  let lastQuery = '';

  function words(language) { return translations[String(language || document.documentElement.lang).startsWith('de') ? 'de' : 'en']; }
  function element(tag, properties = {}, text) {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(properties)) {
      if (key === 'className') node.className = value;
      else node.setAttribute(key, String(value));
    }
    if (text !== undefined) node.textContent = text;
    return node;
  }

  const css = `
    :host { all:initial; position:fixed; z-index:2147483600; display:block;
      font:14px/1.4 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; color:#202938; }
    * { box-sizing:border-box; } button,input,a { font:inherit; } button { -webkit-appearance:none; cursor:pointer; }
    .panel { width:420px; max-width:calc(100vw - 24px); height:492px; max-height:calc(100vh - 28px);
      display:flex; flex-direction:column; overflow:hidden; background:#fff; border:1px solid #e3e7ed;
      border-radius:16px; box-shadow:0 12px 44px #13233c29,0 2px 8px #13233c0d; }
    .header { display:flex; align-items:center; justify-content:space-between; padding:15px 16px 10px; }
    h2 { margin:0; font-size:17px; font-weight:650; letter-spacing:-.3px; }
    .close { width:28px; height:28px; border:0; border-radius:7px; color:#657085; background:transparent; font-size:23px; line-height:22px; }
    .close:hover { background:#f0f3f8; color:#202938; }
    .search-wrap { position:relative; padding:0 14px; }
    .search-icon { position:absolute; width:17px; height:17px; top:11px; left:26px; color:#8390a3; pointer-events:none; }
    input { width:100%; height:39px; outline:0; border:1px solid #e4e9ef; border-radius:9px; padding:0 12px 0 37px;
      background:#f6f8fb; color:#202938; font-size:14px; }
    input::placeholder { color:#7c8799; } input:focus { border-color:#3883e4; box-shadow:0 0 0 2px #3883e420; }
    .categories { display:flex; gap:6px; overflow-x:auto; flex-shrink:0; padding:11px 14px; scrollbar-width:none; }
    .categories::-webkit-scrollbar { display:none; }
    .chip { white-space:nowrap; border:1px solid #e5eaf0; padding:4px 9px; background:#fff; color:#687487; border-radius:7px; font-size:12px; }
    .chip:hover,.chip[aria-pressed=true] { background:#edf5ff; border-color:#c9defa; color:#1668c5; }
    .content { overflow:auto; flex:1; min-height:0; padding:0 12px 10px; overscroll-behavior:contain; scrollbar-width:thin; }
    .grid { display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:6px; }
    .gif { position:relative; display:block; padding:0; overflow:hidden; width:100%; aspect-ratio:1; border:0; border-radius:8px; background:#edf0f5; }
    .gif img { display:block; width:100%; height:100%; object-fit:cover; transition:transform .16s ease; }
    .gif:hover img { transform:scale(1.05); }
    .gif::after { content:"+"; position:absolute; right:6px; bottom:6px; border-radius:50%; width:23px; height:23px;
      font-size:20px; line-height:21px; background:#fff; color:#256ac4; opacity:0; box-shadow:0 1px 5px #0002; }
    .gif:hover::after,.gif:focus-visible::after { opacity:1; }
    button:focus-visible,a:focus-visible { outline:2px solid #267ce0; outline-offset:2px; }
    button:disabled { cursor:wait; } .grid[aria-busy=true] { opacity:.55; pointer-events:none; }
    .message { min-height:190px; display:flex; flex-direction:column; align-items:center; justify-content:center; gap:12px;
      text-align:center; color:#788497; padding:24px 16px; font-size:13px; }
    .retry { border:1px solid #cedcf0; color:#246dca; background:#f3f8ff; border-radius:7px; padding:7px 12px; }
    .skeleton { aspect-ratio:1; border-radius:8px; background:linear-gradient(100deg,#edf1f6 30%,#f8faff 50%,#edf1f6 70%);
      background-size:220% 100%; animation:shimmer 1.3s linear infinite; }
    .footer { display:flex; flex-direction:column; align-items:center; gap:4px; flex-shrink:0; border-top:1px solid #edf0f5; padding:9px 12px 10px; }
    .status { color:#8590a0; font-size:11px; min-height:15px; text-align:center; }
    .powered { color:#627086; font-size:10px; letter-spacing:.45px; text-decoration:none; }
    .powered strong { font-size:11px; font-weight:800; letter-spacing:1px; color:#2e3543; }
    @keyframes shimmer { to { background-position:-220% 0; } }
    @media(prefers-reduced-motion:reduce) { .skeleton { animation:none; } .gif img { transition:none; } }
  `;

  function close({ restoreFocus = true } = {}) {
    if (!active) return;
    const current = active;
    active = null;
    current.closed = true;
    clearTimeout(current.timer);
    current.abort.abort();
    current.resize.disconnect();
    current.host.remove();
    current.anchor?.setAttribute('aria-expanded', 'false');
    window.teamwireGifAPI?.cancel().catch(() => {});
    if (restoreFocus && current.anchor?.isConnected) current.anchor.focus();
  }

  function open(options) {
    if (active) {
      const sameAnchor = active.anchor === options.anchor;
      close();
      if (sameAnchor) return;
    }
    if (!window.teamwireGifAPI || typeof options.onSelectedFiles !== 'function') return;
    const t = words(options.language);
    const host = element('div', { 'data-teamwire-gif-picker': '' });
    const shadow = host.attachShadow({ mode: 'open' });
    const style = element('style'); style.textContent = css;
    const panel = element('section', { className: 'panel', role: 'dialog', 'aria-label': t.open, 'aria-modal': 'false' });
    const header = element('div', { className: 'header' });
    const closeButton = element('button', { className: 'close', type: 'button', 'aria-label': t.close }, '×');
    header.append(element('h2', {}, t.title), closeButton);
    const searchWrap = element('div', { className: 'search-wrap' });
    const icon = element('span', { className: 'search-icon', 'aria-hidden': 'true' });
    icon.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 4.5 4.5"/></svg>';
    const search = element('input', { type: 'search', placeholder: t.search, 'aria-label': t.search, maxlength: 100, autocomplete: 'off', spellcheck: 'false' });
    search.value = lastQuery;
    searchWrap.append(icon, search);
    const categories = element('div', { className: 'categories', 'aria-label': 'GIF categories' });
    const content = element('div', { className: 'content' });
    const footer = element('div', { className: 'footer' });
    const status = element('div', { className: 'status', role: 'status', 'aria-live': 'polite' }, t.hint);
    const powered = element('a', { className: 'powered', href: 'https://giphy.com', target: '_blank', rel: 'noopener noreferrer', 'aria-label': t.attribution });
    powered.append(document.createTextNode('Powered by '), element('strong', {}, 'GIPHY'));
    footer.append(status, powered);
    panel.append(header, searchWrap, categories, content, footer);
    shadow.append(style, panel);
    document.body.append(host);
    const abort = new AbortController();
    const current = { host, anchor: options.anchor, contextKey: options.contextKey, closed: false, timer: null,
      abort, resize: new ResizeObserver(position), sequence: 0, selecting: false, results: [], buttons: [] };
    active = current;
    options.anchor?.setAttribute('aria-expanded', 'true');

    function position() {
      if (current.closed) return;
      if (!options.anchor?.isConnected) { close({ restoreFocus: false }); return; }
      const anchor = options.anchor.getBoundingClientRect();
      const bounds = panel.getBoundingClientRect();
      const left = Math.max(12, Math.min(anchor.left - 6, window.innerWidth - bounds.width - 12));
      const top = anchor.top - bounds.height - 10 >= 12 ? anchor.top - bounds.height - 10
        : Math.max(12, Math.min(anchor.bottom + 10, window.innerHeight - bounds.height - 12));
      host.style.left = `${Math.round(left)}px`;
      host.style.top = `${Math.round(top)}px`;
    }

    function message(text, retry = false) {
      const container = element('div', { className: 'message' });
      container.append(element('span', {}, text));
      if (retry) {
        const button = element('button', { className: 'retry', type: 'button' }, t.retry);
        button.addEventListener('click', () => runSearch());
        container.append(button);
      }
      content.replaceChildren(container);
    }

    async function select(result) {
      if (current.selecting || current.closed) return;
      current.selecting = true;
      status.textContent = t.adding;
      const grid = content.querySelector('.grid');
      grid?.setAttribute('aria-busy', 'true');
      current.buttons.forEach(button => button.disabled = true);
      try {
        const fileData = await window.teamwireGifAPI.download(result.id);
        if (current.closed || active !== current || !current.anchor.isConnected) return;
        const file = new File([fileData.bytes], fileData.name, { type: fileData.type });
        await options.onSelectedFiles([file]);
        if (active === current) close({ restoreFocus: false });
      } catch (error) {
        if (!current.closed) {
          status.textContent = String(error?.message || t.error).replace(/^Error invoking remote method '[^']+': (?:Error: )?/, '');
          current.buttons.forEach(button => button.disabled = false);
          grid?.setAttribute('aria-busy', 'false');
        }
      } finally {
        current.selecting = false;
      }
    }

    function renderResults(results) {
      current.buttons = [];
      const grid = element('div', { className: 'grid', 'aria-label': t.title });
      for (const result of results) {
        const label = `${result.title}${result.creator ? ` — ${t.by} ${result.creator}` : ''}`;
        const button = element('button', { className: 'gif', type: 'button', 'aria-label': label, title: label, 'data-gif-id': result.id });
        const image = element('img', { src: result.previewUrl, alt: '', loading: 'lazy', decoding: 'async', referrerpolicy: 'no-referrer' });
        button.append(image);
        button.addEventListener('click', () => select(result));
        button.addEventListener('keydown', event => {
          const index = current.buttons.indexOf(button);
          const moves = { ArrowRight: 1, ArrowLeft: -1, ArrowDown: 3, ArrowUp: -3, Home: -index, End: results.length - index - 1 };
          if (event.key in moves) {
            event.preventDefault();
            current.buttons[Math.max(0, Math.min(results.length - 1, index + moves[event.key]))]?.focus();
          }
        });
        current.buttons.push(button);
        grid.append(button);
      }
      content.replaceChildren(grid);
      content.scrollTop = 0;
    }

    async function runSearch() {
      clearTimeout(current.timer);
      if (current.closed || current.selecting) return;
      const sequence = ++current.sequence;
      const query = search.value.trim();
      lastQuery = query;
      categories.querySelectorAll('button').forEach(button => button.setAttribute('aria-pressed', String(button.textContent.toLowerCase() === query.toLowerCase())));
      status.textContent = t.loading;
      const skeleton = element('div', { className: 'grid', 'aria-hidden': 'true' });
      for (let index = 0; index < 9; index++) skeleton.append(element('div', { className: 'skeleton' }));
      content.replaceChildren(skeleton);
      try {
        const response = await window.teamwireGifAPI.search(query);
        if (current.closed || sequence !== current.sequence) return;
        current.results = response.results;
        if (!response.results.length) { message(t.empty); status.textContent = t.hint; }
        else { renderResults(response.results); status.textContent = `${response.results.length} ${t.results} · ${t.hint}`; }
      } catch {
        if (current.closed || sequence !== current.sequence) return;
        message(t.error, true);
        status.textContent = t.hint;
      }
    }

    for (const category of t.categories) {
      const chip = element('button', { className: 'chip', type: 'button', 'aria-pressed': 'false' }, category);
      chip.addEventListener('click', () => { if (current.selecting) return; search.value = category; runSearch(); });
      categories.append(chip);
    }
    search.addEventListener('input', () => {
      if (current.selecting) return;
      ++current.sequence;
      current.buttons.forEach(button => button.disabled = true);
      clearTimeout(current.timer);
      current.timer = setTimeout(runSearch, 350);
    });
    search.addEventListener('keydown', event => {
      if (event.key === 'Enter') { event.preventDefault(); runSearch(); }
      if (event.key === 'ArrowDown' && current.buttons.length) { event.preventDefault(); current.buttons[0].focus(); }
    });
    closeButton.addEventListener('click', () => close());
    host.addEventListener('keydown', event => {
      event.stopPropagation();
      if (event.key === 'Escape') { event.preventDefault(); close(); return; }
      if (event.key === 'Tab') {
        const focusable = [...shadow.querySelectorAll('button:not([disabled]),input,a[href]')];
        const index = focusable.indexOf(shadow.activeElement);
        if (event.shiftKey && index <= 0) { event.preventDefault(); focusable.at(-1)?.focus(); }
        else if (!event.shiftKey && index === focusable.length - 1) { event.preventDefault(); focusable[0]?.focus(); }
      }
    });
    document.addEventListener('pointerdown', event => {
      if (!event.composedPath().includes(host) && !event.composedPath().includes(current.anchor)) close({ restoreFocus: false });
    }, { capture: true, signal: abort.signal });
    window.addEventListener('resize', position, { signal: abort.signal });
    document.addEventListener('scroll', position, { capture: true, passive: true, signal: abort.signal });
    current.resize.observe(panel);
    position();
    search.focus();
    runSearch();
  }

  window.teamwireGifPicker = Object.freeze({
    open,
    close,
    label: language => words(language).open,
    closeForContext: contextKey => { if (active?.contextKey === contextKey) close({ restoreFocus: false }); },
  });
})();
