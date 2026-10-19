/* Architecture board page: loads board.json into the renderer, then polls the
   mapping state, the live overlay and the sidebar every 5 s while the tab is
   visible. Every string from the server is set as text or escaped. */
(() => {
  'use strict';
  const page = document.querySelector('[data-board-page]');
  if (!page) return;
  const id = page.dataset.target || '';
  const base = '/projects/' + encodeURIComponent(id);
  const host = page.querySelector('[data-board-root]');
  const statusText = page.querySelector('[data-board-status]');
  const statusDot = page.querySelector('[data-board-dot]');
  const POLL_MS = 5000;
  let api = null;
  let mappedAt = null;
  let emptyKey = null;
  let busy = false;

  const esc = value => String(value == null ? '' : value).replace(/[&<>"']/g, c => ({'&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'}[c]));

  async function getJson(url) {
    const response = await fetch(url, {cache: 'no-store', headers: {Accept: 'application/json'}});
    let body = null;
    try { body = await response.json(); } catch { body = null; }
    return {status: response.status, ok: response.ok, body};
  }

  function ago(iso) {
    const date = new Date(iso || NaN);
    if (!Number.isFinite(date.getTime())) return 'at an unknown time';
    const minutes = Math.round((Date.now() - date.getTime()) / 60000);
    if (minutes < 1) return 'just now';
    if (minutes < 60) return `${minutes}m ago`;
    if (minutes < 2880) return `${Math.round(minutes / 60)}h ago`;
    return `${Math.round(minutes / 1440)}d ago`;
  }

  const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;

  function kindOf(view) {
    const state = (view.state && view.state.state) || 'none';
    if ((state === 'mapping' || state === 'refreshing') && view.running) return state;
    if (state === 'failed') return 'failed';
    if (!view.mapped_at) return 'none';
    if (view.check && view.check.action === 'refresh') return 'drifted';
    return 'ready';
  }

  function stripText(kind, view) {
    const state = view.state || {};
    switch (kind) {
      case 'mapping':
        return `Mapping in background · started ${ago(state.started_at)} · the session did not wait`;
      case 'refreshing':
        return `Refreshing ${plural((state.affected || []).length, 'card')} in background`;
      case 'failed':
        return `Last mapping failed: ${state.error || 'unknown error'} · next session start retries`;
      case 'drifted': {
        const d = (view.check && view.check.drift_counts) || {};
        return `Drifted · ${d.changed || 0} changed, ${d.added || 0} new, ${d.deleted || 0} deleted file(s) since mapping · next session start refreshes`;
      }
      case 'ready': {
        const cost = state.usage && typeof state.usage.cost_usd === 'number' ? ` · last run $${state.usage.cost_usd.toFixed(2)}` : '';
        return `Ready · mapped ${ago(view.mapped_at)} · stored in the harness DB, nothing in the repo${cost}`;
      }
      default:
        return 'Not mapped yet';
    }
  }

  function setStrip(kind, text) {
    statusText.textContent = text;
    statusDot.className = 'state-dot state-dot--' + kind;
  }

  function command() {
    const cmd = `harness board map --target ${id}`;
    return `<div class="board-command"><code>${esc(cmd)}</code><button type="button" class="button" data-copy="${esc(cmd)}">Copy</button></div>`;
  }

  function showEmpty(view) {
    const kind = view ? kindOf(view) : 'none';
    const log = (view && view.log) || '';
    const error = (view && view.state && view.state.error) || '';
    const key = [kind, log, error].join('\u0000');
    if (key === emptyKey) return;
    emptyKey = key;
    let html;
    if (kind === 'mapping' || kind === 'refreshing') {
      html = '<h2>Mapping…</h2><p>A read-only background agent is reading this project; the board appears here when it finishes.</p>' +
        (log ? `<pre class="board-log">${esc(log)}</pre>` : '');
    } else if (kind === 'failed') {
      html = `<h2>Mapping failed</h2><p>${esc(error || 'The last mapping did not finish.')} The next session start in this project retries. To map it now:</p>${command()}` +
        (log ? `<pre class="board-log">${esc(log)}</pre>` : '');
    } else {
      html = `<h2>Not mapped yet</h2><p>The harness maps this project in the background at the next session start in it. To map it now:</p>${command()}`;
    }
    host.innerHTML = `<div class="board-message">${html}</div>`;
  }

  async function loadBoard() {
    const response = await getJson(base + '/board.json');
    if (response.status === 404) return false;
    if (!response.ok || !response.body) throw new Error(`board.json returned HTTP ${response.status}`);
    host.replaceChildren();
    const canvas = document.createElement('div');
    canvas.className = 'board-canvas';
    host.appendChild(canvas);
    api = window.ArchitectureBoard.mount(canvas, response.body);
    mappedAt = response.body.mapped_at || null;
    emptyKey = null;
    return true;
  }

  function sidebarItem(project) {
    const li = document.createElement('li');
    const a = document.createElement('a');
    a.href = '/projects/' + encodeURIComponent(project.id) + '/board';
    a.title = `${project.name} · ${project.label}`;
    if (project.id === id) a.setAttribute('aria-current', 'page');
    const dot = document.createElement('span');
    dot.className = 'state-dot state-dot--' + String(project.state).replace(/[^a-z]/g, '');
    dot.setAttribute('aria-hidden', 'true');
    const name = document.createElement('span');
    name.className = 'rail-projects__name';
    name.textContent = project.name;
    const label = document.createElement('span');
    label.className = 'rail-projects__label';
    label.textContent = project.label;
    a.append(dot, name, label);
    li.appendChild(a);
    return li;
  }

  async function refreshSidebar() {
    const list = document.querySelector('[data-board-sidebar]');
    if (!list) return;
    const response = await getJson('/projects/sidebar.json');
    if (!response.ok || !Array.isArray(response.body)) return;
    if (!response.body.length) {
      const empty = document.createElement('li');
      empty.className = 'rail-projects__empty';
      empty.textContent = 'No projects yet';
      list.replaceChildren(empty);
      return;
    }
    list.replaceChildren(...response.body.map(sidebarItem));
  }

  async function tick() {
    if (busy || document.visibilityState !== 'visible') return;
    busy = true;
    try {
      const state = await getJson(base + '/state.json');
      if (state.ok && state.body) {
        const view = state.body;
        const kind = kindOf(view);
        if (api && view.mapped_at && view.mapped_at !== mappedAt) {
          // A new mapping finished: the renderer binds window listeners, so reload rather than mount twice.
          location.reload();
          return;
        }
        if (!api && view.mapped_at) await loadBoard();
        if (!api) showEmpty(view);
        setStrip(kind, stripText(kind, view));
        if (api) {
          api.setStatus({kind, text: kind});
          const overlay = await getJson(base + '/overlay.json');
          if (overlay.ok && overlay.body) api.setOverlay(overlay.body);
        }
      } else {
        if (!api) showEmpty(null);
        setStrip('failed', `Status unavailable (HTTP ${state.status}) · retrying`);
      }
      await refreshSidebar();
    } catch (error) {
      if (!api && emptyKey === null) showEmpty(null);
      setStrip('failed', 'Status unavailable · retrying');
    } finally {
      busy = false;
    }
  }

  host.addEventListener('click', event => {
    const button = event.target.closest('[data-copy]');
    if (!button) return;
    const text = button.dataset.copy;
    const done = () => { button.textContent = 'Copied'; setTimeout(() => { button.textContent = 'Copy'; }, 1500); };
    const fallback = () => {
      const area = document.createElement('textarea');
      area.value = text;
      document.body.appendChild(area);
      area.select();
      try { document.execCommand('copy'); done(); } catch { /* the command stays visible to copy by hand */ }
      area.remove();
    };
    if (navigator.clipboard && window.isSecureContext) navigator.clipboard.writeText(text).then(done, fallback);
    else fallback();
  });

  (async () => {
    try {
      await loadBoard();
    } catch (error) {
      host.innerHTML = `<div class="board-message"><h2>The board could not be loaded</h2><p>${esc(error.message)}</p></div>`;
      emptyKey = 'error';
    }
    await tick();
    setInterval(tick, POLL_MS);
    document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'visible') tick(); });
  })();
})();
