/* Architecture board page: loads board.json into the renderer, then polls the
   mapping state and the live overlay every 5 s while the tab is
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
  const csrf = page.dataset.csrf || '';
  const POLL_MS = 5000;
  const FINGERPRINT = /^[0-9a-f]{64}$/;
  const sessionKey = page.dataset.sessionKey || '';
  const sessionBase = sessionKey ? '/projects/' + encodeURIComponent(page.dataset.sessionTarget || '') + '/sessions/' + encodeURIComponent(sessionKey) : '';
  const deepLink = (new URLSearchParams(location.search).get('job') || '').toLowerCase();
  let deepLinkDone = !FINGERPRINT.test(deepLink);
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

  /* POST a form with the session token; a refused request throws its server message. */
  async function postForm(url, fields) {
    const form = new URLSearchParams(Object.assign({csrf_token: csrf}, fields));
    const response = await fetch(url, {method: 'POST', cache: 'no-store', headers: {Accept: 'application/json'}, body: form});
    let body = null;
    try { body = await response.json(); } catch { body = null; }
    if (!response.ok) throw new Error((body && body.error && body.error.message) || `HTTP ${response.status}`);
    return body;
  }

  /* Issue agents: the board renders the job panel; these calls are the only way it reaches the server. */
  const agents = {
    async act(request) {
      const fp = request.fingerprint;
      if (request.act === 'analyze' || request.act === 'reanalyze') {
        await postForm(base + '/agents', {issue_key: request.issue, card_id: request.card, reanalyze: request.act === 'reanalyze' ? 'true' : 'false'});
      } else {
        const path = {stop: 'stop', takeover: 'takeover', handback: 'handback', 'difit-start': 'difit/start', 'difit-stop': 'difit/stop'}[request.act];
        if (!path) throw new Error('unknown action');
        const body = await postForm(`${base}/agents/${fp}/${path}`, {});
        if (api && body && body.job && body.resume_commands) api.setJob(fp, body);
      }
      await Promise.all([loadJob(fp), loadAgents()]);
    },
    load(fp) { loadJob(fp); },
    difitHref: fp => `${base}/agents/${fp}/difit`,
  };

  async function loadJob(fp) {
    if (!api || !FINGERPRINT.test(fp)) return;
    const response = await getJson(`${base}/agents/${fp}.json`);
    if (response.ok && response.body) api.setJob(fp, response.body);
  }

  async function loadAgents() {
    if (!api) return;
    const response = await getJson('/agents.json?target=' + encodeURIComponent(id));
    if (!response.ok || !response.body) return;
    api.setAgents(response.body.jobs || [], {generated_at: response.body.generated_at, runner: response.body.runner || null});
    if (!deepLinkDone) deepLinkDone = api.openJob(deepLink);
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
    api = window.ArchitectureBoard.mount(canvas, response.body, {agents, target: id});
    mappedAt = response.body.mapped_at || null;
    emptyKey = null;
    return true;
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
          await loadAgents();
          await Promise.all(api.watched().map(loadJob));
        }
      } else {
        if (!api) showEmpty(null);
        setStrip('failed', `Status unavailable (HTTP ${state.status}) · retrying`);
      }
    } catch (error) {
      if (!api && emptyKey === null) showEmpty(null);
      setStrip('failed', 'Status unavailable · retrying');
    } finally {
      busy = false;
    }
  }

  host.addEventListener('click', event => {
    const button = event.target.closest('.board-message [data-copy]');
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

  /* The session page: the owner's focus document, then the shared map; focus refreshes every 10 s. */
  async function sessionMain() {
    const FOCUS_MS = 10000;
    let sessionApi = null;
    const say = (kind, text) => { statusText.textContent = text; statusDot.className = 'state-dot state-dot--' + kind; };
    const stop = (title, text) => { host.innerHTML = `<div class="board-message"><h2>${esc(title)}</h2><p>${esc(text)}</p></div>`; };
    async function refresh() {
      if (document.visibilityState !== 'visible') return;
      try {
        const response = await getJson(sessionBase + '/focus.json');
        if (response.status === 404) { say('failed', 'This session is not available'); return; }
        if (!response.ok || !response.body) throw new Error('HTTP ' + response.status);
        if (sessionApi) sessionApi.setFocus(response.body);
        const session = response.body.session || {};
        say(session.live ? 'ready' : 'none', session.live ? 'Live · refreshing every 10 s' : 'Not running now · last read ' + ago(session.last_active));
      } catch (error) {
        say('failed', 'Session unavailable · retrying');
      }
    }
    const first = await getJson(sessionBase + '/focus.json');
    if (!first.ok || !first.body) { stop('Not found', 'This session is not available to you.'); say('failed', 'Not found'); return; }
    const map = await getJson(base + '/board.json');
    if (map.status === 404) { stop('The map is not ready', 'The main folder has no architecture board yet. The harness maps it in the background at the next session start in it.'); say('none', 'No map yet'); return; }
    if (!map.ok || !map.body) { stop('The board could not be loaded', 'board.json returned HTTP ' + map.status); say('failed', 'Board unavailable'); return; }
    host.replaceChildren();
    const canvas = document.createElement('div');
    canvas.className = 'board-canvas';
    host.appendChild(canvas);
    sessionApi = window.ArchitectureBoard.mount(canvas, map.body, {session: true, target: id});
    sessionApi.setFocus(first.body);
    const session = first.body.session || {};
    say(session.live ? 'ready' : 'none', session.live ? 'Live · refreshing every 10 s' : 'Not running now · last read ' + ago(session.last_active));
    setInterval(refresh, FOCUS_MS);
    document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'visible') refresh(); });
  }

  if (sessionKey) { sessionMain(); return; }

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
