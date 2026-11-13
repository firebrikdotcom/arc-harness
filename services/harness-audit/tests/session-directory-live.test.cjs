const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const script = fs.readFileSync(`${__dirname}/../resources/views/components/session-directory-live.html`, 'utf8').replace(/^<script>\s*|\s*<\/script>\s*$/g, '');

function fixture() {
 const state = {calls: [], replacements: [], localized: 0, hover: false};
 const form = {value: 'unsubmitted search', focused: true};
 const regions = ['summary', 'chips', 'results'].map(name => ({
  dataset: {directoryLive: name},
  contains: element => element === state.focus,
  querySelector: () => state.hover,
  replaceWith: replacement => state.replacements.push(replacement),
 }));
 const document = {hidden: false, activeElement: form, querySelectorAll: () => regions, addEventListener: (_, handler) => { state.visible = handler; }};
 const location = {href: 'http://localhost/workflow?agent=Codex&sort=latest&page=2&per_page=10'};
 vm.runInNewContext(script, {
  document, location, AbortController,
  setTimeout: () => 1, clearTimeout: () => {},
  setInterval: callback => { state.tick = callback; },
  window: {localizeClientTimes: () => { state.localized++; }},
  DOMParser: class { parseFromString(text) { return {querySelector: selector => ({selector, text})}; } },
  fetch: async (url, options) => { state.calls.push({url, options}); if (state.fail) throw Error('offline'); if (state.wait) await state.wait; return {ok: true, text: async () => 'older session now first'}; },
 });
 return {state, document, location, form};
}

test('refresh uses applied filters, sort and page while retaining focused unsaved form edits', async () => {
 const {state, location, form, document} = fixture();
 await state.tick();
 assert.equal(state.calls[0].url, location.href);
 assert.equal(state.calls[0].options.cache, 'no-store');
 assert.equal(state.replacements.length, 3);
 assert.equal(state.replacements[2].text, 'older session now first');
 assert.equal(state.localized, 1);
 assert.equal(form.value, 'unsubmitted search');
 assert.equal(document.activeElement, form);
});

test('hidden tabs, focused links and hovered regions defer updates; becoming visible refreshes', async () => {
 const {state, document} = fixture();
 document.hidden = true; await state.tick();
 document.hidden = false; state.focus = document.activeElement; await state.tick();
 state.focus = null; state.hover = true; await state.tick();
 assert.equal(state.calls.length, 0);
 state.hover = false; state.visible();
 await new Promise(resolve => setImmediate(resolve));
 assert.equal(state.replacements.length, 3);
});

test('requests never overlap and discard responses if navigation or focus changes', async () => {
 const {state, location, document} = fixture();
 let resolve; state.wait = new Promise(done => { resolve = done; });
 const first = state.tick(); await state.tick();
 assert.equal(state.calls.length, 1);
 location.href += '&q=changed'; resolve(); await first;
 assert.equal(state.replacements.length, 0);
 state.wait = null; state.fail = true; await state.tick();
 assert.equal(state.replacements.length, 0);
 state.fail = false;
 state.wait = new Promise(done => { resolve = done; });
 const second = state.tick(); state.focus = document.activeElement; resolve(); await second;
 assert.equal(state.replacements.length, 0);
 state.focus = null; state.wait = null; await state.tick();
 assert.equal(state.replacements.length, 3);
});
