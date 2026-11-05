"""Issue-agent UI on the architecture board, driven in headless Chrome.

The page inlines board.css, board.js and the fixture board, mounts the board
with a fake `agents` client, clicks through Analyze, take-over and the copy
buttons, and prints its findings as JSON into the DOM that Chrome dumps.
Nothing reaches a server, an agent, or the person's Chrome profile.
"""

import html
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BOARD = ROOT / "tools" / "architecture-board"
MAC_CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"


def find_chrome():
    explicit = os.environ.get("HARNESS_TEST_CHROME")
    if explicit:
        return explicit if os.access(explicit, os.X_OK) else None
    for name in ("chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "chrome"):
        found = shutil.which(name)
        if found:
            return found
    return MAC_CHROME if os.access(MAC_CHROME, os.X_OK) else None


CHROME = find_chrome()

SCENARIO = r"""
(async () => {
  const FP = "a".repeat(64), OTHER = "b".repeat(64);
  const out = {checks: {}, errors: []};
  const check = (name, ok, detail) => { out.checks[name] = ok ? true : String(detail === undefined ? "failed" : detail); };
  const wait = () => new Promise(r => setTimeout(r, 30));
  const copied = [];
  try {
    Object.defineProperty(navigator, "clipboard", {configurable: true, value: {writeText: t => { copied.push(t); return Promise.resolve(); }}});
  } catch (e) { out.errors.push("clipboard stub: " + e); }
  const job = extra => Object.assign({
    fingerprint: FP, target_id: "demo-1", card_id: "app", issue_key: "nw/1", issue_title: "boom",
    status: "running", owner: "runner", agent: "claude", session_id: "5e551011-0000-4000-8000-000000000001",
    worktree: {path: "/tmp/wt/" + FP.slice(0, 12), branch: "harness/analyze-" + FP.slice(0, 12), base_branch: "main"},
    machine: "build-box", launcher: "background", herdr_tab: null, difit: {port: null}, error: null,
    last_activity_at: new Date().toISOString(), events: [{at: "2026-10-25T10:00:00Z", type: "started", summary: "agent <b>started</b>"},
      // A real event names the root frame: one long unbreakable path, which once widened the panel past the drawer.
      {at: "2026-10-25T10:00:01Z", type: "queued", summary: "Analyze queued for app/Services/RevenueCatSubscriptions/RevenueCatMemberBillingLockCoordinator.php:113"}],
  }, extra || {});
  const calls = [];
  let api;
  const agents = {
    async act(req) {
      calls.push(req);
      if (req.act === "stop") throw new Error("job is <busy>");
      if (req.act === "analyze") api.setJob(req.fingerprint, {job: job(), difit_running: false, diagnosis: null, resume_commands: null, takeover_wait_seconds: 45});
      if (req.act === "takeover") api.setJob(req.fingerprint, {job: job({status: "stopped", owner: "you"}), difit_running: false,
        resume_commands: {local: "cd /tmp/wt && claude --resume 5e551011", ssh: null, herdr: {tab_id: null, command: "cd /tmp/wt && claude --resume 5e551011", note: "open a Herdr tab"}}});
    },
    load(fp) { calls.push({act: "load", fingerprint: fp}); },
    difitHref: fp => "/projects/demo-1/agents/" + fp + "/difit",
  };
  const overlay = {edited: {}, scope: [], refreshing: [], heat: {cards: {app: {heat: 1, issues: 2, top: [
    {key: "nw/1", title: "<img src=x onerror=window.__xss=1>", level: "error", count: 3, fingerprint: FP},
    {key: "nw/2", title: "no root cause", level: "warning", count: 1},
  ]}}}};

  try {
  const root = document.getElementById("board");
  api = window.ArchitectureBoard.mount(root, window.FIXTURE, {agents});
  api.setOverlay(overlay);
  api.select("app");
  await wait();
  const panel = root.querySelector(".ab-panel");
  const slot = () => panel.querySelector(`[data-slot="${FP}"]`);
  const btn = act => slot() && slot().querySelector(`[data-agent="${act}"]`);
  const badge = () => root.querySelector('.ab-card[data-id="app"] .ab-badge--agent, [data-id="app"] .ab-badge--agent');

  check("one_slot_only_for_a_fingerprint", panel.querySelectorAll("[data-slot]").length === 1, panel.querySelectorAll("[data-slot]").length);
  check("analyze_button", !!btn("analyze"));
  check("title_escaped", !panel.querySelector("img") && panel.textContent.includes("<img src=x"));

  btn("analyze").click();
  await wait();
  const analyze = calls.find(c => c.act === "analyze");
  check("analyze_request", analyze && analyze.fingerprint === FP && analyze.issue === "nw/1" && analyze.card === "app", JSON.stringify(analyze));
  check("chip_analyzing", slot().querySelector(".ab-jchip.run") && slot().textContent.includes("Analyzing"), slot().textContent);
  check("job_panel_open", !!slot().querySelector(".ab-job") && slot().textContent.includes("Analyze job " + FP.slice(0, 8)));
  check("event_escaped", !slot().querySelector(".ab-feed b") && slot().textContent.includes("agent <b>started</b>"));
  check("card_badge", badge() && badge().textContent.includes("agent · 1"), badge() ? badge().textContent : "missing");
  check("stop_offered", !!btn("stop"));

  const toggle = btn("toggle");
  toggle.focus();
  api.setAgents([job()]);
  api.setOverlay(overlay);
  await wait();
  check("unchanged_slot_keeps_focus", document.activeElement === toggle);

  const copyButton = slot().querySelector(".ab-jcopy");
  copyButton.click();
  await wait();
  const toast = root.querySelector(".ab-toast");
  check("copy_session", copied.includes(job().session_id) || (toast && toast.textContent.includes(job().session_id)), JSON.stringify(copied));

  btn("stop").click();
  await wait();
  check("error_note_escaped", slot().querySelector(".ab-jnote") && slot().textContent.includes("job is <busy>") && !slot().querySelector(".ab-jnote busy"));

  btn("takeover").click();
  await wait();
  check("takeover_confirms_first", !!slot().querySelector(".ab-jconfirm") && !calls.some(c => c.act === "takeover"));
  const confirmText = slot().querySelector(".ab-jconfirm").textContent;
  check("takeover_waits_for_the_step", confirmText.includes("The background agent stops after its current step (at most 45 s), then its session is yours to resume."), confirmText);
  btn("takeover-yes").click();
  await wait();
  check("takeover_request", calls.some(c => c.act === "takeover" && c.fingerprint === FP));
  check("chip_yours", !!slot().querySelector(".ab-jchip.you") && slot().textContent.includes("Yours"), slot().textContent);
  check("hand_back_offered", !!btn("handback"));
  check("resume_command", slot().querySelector(".ab-cmd") && slot().querySelector(".ab-cmd").textContent.includes("claude --resume"));
  check("ssh_tab_disabled_without_config", slot().querySelector('[data-tab="ssh"]').disabled === true);
  check("no_badge_when_not_running", !badge());

  api.setJob(FP, {job: job({status: "diagnosed", owner: "runner"}), difit_running: true, resume_commands: null,
    diagnosis: {sections: [{heading: "Root cause", text: "<script>window.__xss=2<\/script> null deref"}, {heading: "Confidence", text: "high"}]}});
  await wait();
  check("chip_diagnosed", slot().querySelector(".ab-jchip.ok") && slot().textContent.includes("Diagnosis ready"));
  check("diagnosis_escaped", !slot().querySelector(".ab-diag script") && slot().textContent.includes("<script>window.__xss=2<\/script>"));
  check("confidence_in_kicker", slot().querySelector(".ab-diag .kicker").textContent.includes("confidence high"));
  const open = slot().querySelector('a[href$="/difit"]');
  check("difit_open_link", open && open.getAttribute("href") === "/projects/demo-1/agents/" + FP + "/difit" && open.rel.includes("noopener"));
  check("reanalyze_offered", !!btn("reanalyze"));
  api.setAgents([job({status: "running", stopping: "2026-10-26T10:00:00Z"})]);
  api.setJob(FP, {job: job({status: "running", stopping: "2026-10-26T10:00:00Z"}), difit_running: false});
  await wait();
  check("chip_stopping", slot().querySelector(".ab-jchip.run") && slot().textContent.includes("Stopping"), slot().textContent);
  api.setJob(FP, {job: job({status: "running", owner: "you", stopping: "2026-10-26T10:00:00Z"}), difit_running: false, takeover_wait_seconds: 45,
    resume_commands: {local: "cd /tmp/wt && claude --resume 5e551011", ssh: null, herdr: {tab_id: null, command: "x", note: "y"}}});
  await wait();
  check("takeover_shows_stopping", slot().querySelector(".ab-jchip.run") && slot().textContent.includes("Stopping")
    && slot().textContent.includes("at most 45 s") && !btn("handback") && !slot().querySelector(".ab-cmd"), slot().textContent);
  api.setJob(FP, {job: job({status: "detached", error: "the agent may still be working in Herdr tab w1-t7"}), difit_running: false});
  await wait();
  check("chip_detached", slot().querySelector(".ab-jchip.hot") && slot().textContent.includes("Detached")
    && slot().textContent.includes("may still be working"), slot().textContent);

  // Observation closed a taken-over job: it says Ended, not Yours, and offers a new analysis.
  api.setJob(FP, {job: job({status: "ended", owner: "you", events: [{at: "2026-10-27T10:00:00Z", type: "ended",
    summary: "Herdr no longer has agent issue-aaaaaaaa and its tab is gone (pane w1:p7 not found); the session holds no diagnosis"}]}),
    difit_running: false, resume_commands: {local: "cd /tmp/wt && claude --resume 5e551011", ssh: null, herdr: {tab_id: null, command: "x", note: "y"}}});
  await wait();
  check("chip_ended", slot().querySelector(".ab-jchip.end") && slot().textContent.includes("Ended") && !slot().textContent.includes("Yours"), slot().textContent);
  check("ended_says_how", slot().textContent.includes("its tab is gone"), slot().textContent);
  check("ended_offers_reanalyze", !!btn("reanalyze") && !btn("handback") && !btn("stop"));
  check("ended_card_has_no_badge", !badge());

  // An unfinished job whose session has been quiet for over ten minutes says so.
  api.setJob(FP, {job: job({last_activity_at: new Date(Date.now() - 25 * 60000).toISOString(), observed_at: new Date().toISOString()}), difit_running: false});
  await wait();
  check("no_activity_text", slot().querySelector(".ab-jquiet") && slot().textContent.includes("No activity for 25 min"), slot().textContent);
  check("observed_shown", slot().querySelector(".ab-jhead small").textContent.includes("checked just now"), slot().querySelector(".ab-jhead small").textContent);
  api.setJob(FP, {job: job({last_activity_at: new Date(Date.now() - 9 * 60000).toISOString()}), difit_running: false});
  await wait();
  check("no_activity_only_past_ten_minutes", !slot().querySelector(".ab-jquiet"), slot().textContent);
  api.setJob(FP, {job: job({status: "failed", last_activity_at: new Date(Date.now() - 60 * 60000).toISOString()}), difit_running: false});
  await wait();
  check("no_activity_only_while_unfinished", !slot().querySelector(".ab-jquiet"), slot().textContent);
  api.setJob(FP, {job: job(), difit_running: false});
  await wait();

  // The signals service has not ticked: a banner, judged on the server's clock.
  const banner = root.querySelector('[data-r="banner"]');
  check("no_banner_without_runner_info", banner.hidden);
  api.setAgents([job()], {generated_at: "2026-10-27T10:10:00Z", runner: {ticked_at: "2026-10-27T10:05:30Z"}});
  await wait();
  check("stale_tick_banner", !banner.hidden && banner.getAttribute("role") === "alert"
    && banner.textContent.includes("has not checked issue agents for 4 min"), banner.textContent);
  api.setAgents([job()], {generated_at: "2026-10-27T10:10:00Z", runner: {ticked_at: "2026-10-27T10:08:30Z"}});
  await wait();
  check("fresh_tick_no_banner", banner.hidden, banner.textContent);
  api.setAgents([job()], {generated_at: "2026-10-27T10:10:00Z", runner: null});
  await wait();
  check("never_ticked_banner", !banner.hidden && banner.textContent.includes("never checked"), banner.textContent);
  api.setAgents([job({status: "diagnosed"})], {generated_at: "2026-10-27T10:10:00Z", runner: null});
  await wait();
  check("never_ticked_but_nothing_waits", banner.hidden, banner.textContent);
  api.setAgents([job()], {generated_at: "2026-10-27T10:10:00Z", runner: {ticked_at: "2026-10-27T10:05:30Z"}});
  await wait();
  const bar = root.querySelector(".ab-bar").getBoundingClientRect();
  check("banner_fits_width", bar.right <= window.innerWidth + 1 && banner.getBoundingClientRect().right <= window.innerWidth + 1,
    `bar ${bar.right}, banner ${banner.getBoundingClientRect().right} in ${window.innerWidth}`);
  check("no_fix_control", ![...root.querySelectorAll("button,a")].some(b => /\bfix\b/i.test(b.textContent)), "a Fix control exists");
  check("no_script_ran", !window.__xss);

  if (window.innerWidth <= 820) panel.click();
  await wait();
  const box = slot().querySelector(".ab-job").getBoundingClientRect();
  check("fits_width", box.left >= 0 && box.right <= window.innerWidth + 1 && panel.scrollWidth <= panel.clientWidth + 1,
    `job ${box.left}..${box.right} in ${window.innerWidth}, panel ${panel.scrollWidth}/${panel.clientWidth}`);

  const plain = document.getElementById("plain");
  const exported = window.ArchitectureBoard.mount(plain, window.FIXTURE, {});
  exported.setOverlay(overlay);
  exported.select("app");
  await wait();
  check("export_has_no_agent_ui", !plain.querySelector("[data-slot],[data-agent],.ab-badge--agent"));
  check("unknown_job_not_opened", api.openJob(OTHER) === false);
  } catch (e) { out.errors.push(String(e && e.stack || e)); }
  document.getElementById("result").textContent = JSON.stringify(out);
})();
"""

TICKER = r"""
(async () => {
  const out = {checks: {}, errors: []};
  const check = (name, ok, detail) => { out.checks[name] = ok ? true : String(detail === undefined ? "failed" : detail); };
  const wait = () => new Promise(r => setTimeout(r, 30));
  const FP = "c".repeat(64);
  const iso = m => new Date(Date.now() - m * 60000).toISOString();
  // Read marks for issues absent from every poll: a recent one survives, one older than 14 days is pruned.
  try { localStorage.clear(); localStorage.setItem("ab-read:demo-1", JSON.stringify({
    "nightwatch#8": {s: iso(60), t: Date.now() - 20 * 864e5}, "nightwatch#9": {s: iso(60), t: Date.now() - 864e5}})); } catch (e) {}
  const mk = (key, extra) => Object.assign({key, fingerprint: null, title: "issue " + key, level: "error", count: 2, users: 1,
    first_seen: iso(3000), last_seen: iso(60), url: "https://nightwatch.example/i/" + key, source: "nightwatch:prod", new: false, card: null, frames: []}, extra);
  const frames = [{path: "vendor/laravel/x.php", line: 3, in_app: false}].concat([1, 2, 3, 4, 5].map(n => ({path: "src/app/f" + n + ".php", line: n, in_app: true})));
  const A = mk("nightwatch#1", {title: "Carded <img src=x onerror=window.__xss=1>", card: "app", fingerprint: FP, last_seen: iso(5), first_seen: iso(30), frames});
  const S = mk("nightwatch#2", {title: "GET|HEAD /api/x exceeded 1,000ms threshold", level: "warning", last_seen: iso(20)});
  const C = mk("nightwatch#3", {title: "Old cardless error", last_seen: iso(120)});
  const heat = issues => ({edited: {}, heat: {cards: {app: {heat: 1, issues: 1, top: [Object.assign({}, A)]}}, issues}});
  const calls = [];
  const agents = {async act(req) { calls.push(req); }, load(fp) { calls.push({act: "load", fingerprint: fp}); }, difitHref: fp => "/d/" + fp};
  try {
  const root = document.getElementById("board");
  const api = window.ArchitectureBoard.mount(root, window.FIXTURE, {agents, target: "demo-1"});
  const T = () => root.querySelector(".ab-ticker"), P = root.querySelector(".ab-panel"), SH = () => root.querySelector(".ab-sheet");
  const keys = () => [...T().querySelectorAll(".ab-tk")].map(e => e.dataset.key);
  api.setOverlay(heat([C, A, S]));
  await wait();
  check("ticker_shown", T() && !T().hidden);
  check("newest_first", keys().join() === "nightwatch#1,nightwatch#2,nightwatch#3", keys().join());
  check("counter", T().textContent.includes("1 / 3"), T().textContent);
  const first = T().querySelector(".ab-tk");
  check("item_parts", first.querySelector(".ab-lvl.err") && first.querySelector(".ab-new") && first.textContent.includes("5m ago")
    && !first.classList.contains("read") && first.querySelector(".ab-dot"), first.textContent);
  check("slow_level", T().querySelectorAll(".ab-tk")[1].querySelector(".ab-lvl.slow"));
  check("no_card_label", T().querySelectorAll(".ab-tk")[2].textContent.includes("No card owns it"));
  check("title_escaped", !root.querySelector(".ab-ticker img") && first.textContent.includes("<img"));

  // A wheel over the ticker scrolls it, not the board's zoom.
  const zv = root.querySelector('[data-r="zv"]').textContent;
  T().querySelector(".ab-track").dispatchEvent(new WheelEvent("wheel", {deltaY: 300, bubbles: true, cancelable: true}));
  await wait();
  check("wheel_does_not_zoom", root.querySelector('[data-r="zv"]').textContent === zv);

  // Filter.
  const sel = T().querySelector('[data-tk="filter"]');
  sel.value = "slow"; sel.dispatchEvent(new Event("change", {bubbles: true}));
  await wait();
  check("filter_slow", keys().join() === "nightwatch#2", keys().join());
  const sel2 = T().querySelector('[data-tk="filter"]');
  sel2.value = "all"; sel2.dispatchEvent(new Event("change", {bubbles: true}));
  await wait();

  // Minimise to a pill, remembered, and restore.
  T().querySelector('[data-tk="min"]').click();
  await wait();
  const pill = T().querySelector(".ab-pill");
  check("pill", pill && pill.textContent.includes("3 issues") && pill.textContent.includes("3 unread") && !T().querySelector(".ab-track"), T().textContent);
  check("min_remembered", localStorage.getItem("ab-ticker-min") === "1");
  // A new key arrives while minimised: +1 new on the pill.
  const D = mk("nightwatch#4", {title: "Brand new", last_seen: iso(0)});
  api.setOverlay(heat([C, A, S, D]));
  await wait();
  check("pill_new", T().querySelector(".ab-pill").textContent.includes("+1 new"), T().textContent);
  T().querySelector('[data-tk="restore"]').click();
  await wait();
  check("restore", !!T().querySelector(".ab-track") && localStorage.getItem("ab-ticker-min") === "0");
  check("restore_merges", keys()[0] === "nightwatch#4" && keys().length === 4, keys().join());

  // Live: another arrival and a later last_seen do not reorder; they wait behind "↑ 2 new".
  const before = keys().join();
  const E = mk("nightwatch#5", {title: "Another", last_seen: iso(0)});
  const C2 = Object.assign({}, C, {last_seen: new Date().toISOString(), count: 9});
  api.setOverlay(heat([C2, A, S, D, E]));
  await wait();
  const flushBtn = T().querySelector('[data-tk="flush"]');
  check("new_waits", flushBtn && flushBtn.textContent.includes("↑ 2 new") && keys().join() === before, (flushBtn ? flushBtn.textContent : "none") + " " + keys().join());
  flushBtn.click();
  await wait();
  check("flush_merges_on_top", keys().slice(0, 2).sort().join() === "nightwatch#3,nightwatch#5" && keys().length === 5 && !T().querySelector('[data-tk="flush"]'), keys().join());

  // Pick the carded issue: card selected and pinged, compact summary, sheet open.
  const idxA = keys().indexOf("nightwatch#1");
  T().querySelectorAll(".ab-tk")[idxA].click();
  await wait();
  const card = root.querySelector('.ab-card[data-id="app"]');
  check("card_selected", card.classList.contains("sel") && card.classList.contains("ab-ping"));
  check("compact_summary", P.classList.contains("ab-compact") && P.querySelector(".ab-isum") && P.querySelector(".ab-isum").textContent.includes("#1")
    && getComputedStyle(P.querySelector("h3")).display === "none", P.className);
  check("marked_read", T().querySelectorAll(".ab-tk")[idxA].classList.contains("read") && JSON.parse(localStorage.getItem("ab-read:demo-1"))["nightwatch#1"].s === A.last_seen);
  const marks = JSON.parse(localStorage.getItem("ab-read:demo-1"));
  check("read_marks_pruned_by_age", marks["nightwatch#9"] && !marks["nightwatch#8"], JSON.stringify(marks));
  P.querySelector("[data-icard]").click();
  await wait();
  check("card_details_toggle", !P.classList.contains("ab-compact") && getComputedStyle(P.querySelector("h3")).display !== "none" && P.querySelector("[data-icard]").textContent.includes("▴"));
  P.querySelector("[data-icard]").click();
  await wait();
  check("card_details_fold", P.classList.contains("ab-compact"));

  // Sheet tabs.
  check("sheet_open", SH() && !SH().hidden && SH().textContent.includes("Issue #1") && SH().querySelector('[data-stab="overview"][aria-selected="true"]'));
  check("overview_facts", SH().textContent.includes("Events") && SH().querySelectorAll(".ab-stack li").length === 4 && SH().querySelector('a[href="https://nightwatch.example/i/nightwatch#1"]')
    && SH().textContent.includes("Open in Nightwatch"), SH().textContent);
  const tab = k => { SH().querySelector(`[data-stab="${k}"][role="tab"]`).click(); return wait(); };
  await tab("stack");
  check("stack_tab", SH().querySelectorAll(".ab-stack li").length === 6 && SH().querySelectorAll(".ab-stack li.vendor").length === 1);
  await tab("card");
  check("card_tab", SH().querySelector(".ab-cardref") && SH().querySelector('[data-sh="showcard"]'));
  await tab("agent");
  const an = SH().querySelector('[data-agent="analyze"]');
  check("agent_tab_reuses_slot", an && SH().querySelector(`[data-slot="${FP}"]`));
  an.click();
  await wait();
  check("analyze_request", calls.some(c => c.act === "analyze" && c.fingerprint === FP && c.issue === "nightwatch#1" && c.card === "app"), JSON.stringify(calls));
  await tab("card");
  SH().querySelector('[data-sh="showcard"]').click();
  await wait();
  check("show_on_board_folds", SH().classList.contains("min") && card.classList.contains("ab-ping"));

  // Sheet minimise, restore, close.
  SH().querySelector('[data-sh="restore"]').click();
  await wait();
  check("sheet_restore", !SH().classList.contains("min") && SH().querySelector(".ab-stabs"));
  SH().querySelector('[data-sh="min"]').click();
  await wait();
  check("sheet_min", SH().classList.contains("min") && SH().querySelector(".ab-sheettab") && SH().textContent.includes("Carded"));
  SH().querySelector('[data-sh="close"]').click();
  await wait();
  check("sheet_close", SH().hidden);

  // The slow issue: no card, so the selection clears; the stack tab says it is a threshold alert.
  T().querySelectorAll(".ab-tk")[keys().indexOf("nightwatch#2")].click();
  await wait();
  check("no_card_clears", !root.querySelector(".ab-card.sel") && !root.querySelector(".ab-card.ab-ping") && P.querySelector(".ab-isum") && !P.querySelector("[data-icard]"));
  await tab("stack");
  check("slow_no_stack", SH().textContent.includes("threshold alert"), SH().textContent);
  await tab("agent");
  check("no_analyze_without_root", !SH().querySelector("[data-agent]"));

  // Keys n / p step through the ticker.
  const cur = () => T().querySelector(".ab-tk.cur").dataset.key;
  const c0 = cur();
  dispatchEvent(new KeyboardEvent("keydown", {key: "n"}));
  await wait();
  check("key_n", cur() === keys()[(keys().indexOf(c0) + 1) % keys().length] && cur() !== c0, cur());
  dispatchEvent(new KeyboardEvent("keydown", {key: "p"}));
  await wait();
  check("key_p", cur() === c0);

  // A direct click on a card keeps today's full panel.
  const other = root.querySelector('.ab-card[data-id="engine"]');
  other.dispatchEvent(new PointerEvent("pointerdown", {bubbles: true, pointerId: 7, pointerType: "mouse", button: 0, clientX: 5, clientY: 5}));
  other.dispatchEvent(new PointerEvent("pointerup", {bubbles: true, pointerId: 7, pointerType: "mouse", button: 0, clientX: 5, clientY: 5}));
  await wait();
  check("direct_click_full_panel", other.classList.contains("sel") && !P.classList.contains("ab-compact") && !P.querySelector(".ab-isum")
    && getComputedStyle(P.querySelector("h3")).display !== "none", P.className);

  // Heat off hides the ticker.
  root.querySelector('[data-r="heat"]').click();
  await wait();
  check("heat_off_hides", T().hidden);
  root.querySelector('[data-r="heat"]').click();
  await wait();
  check("heat_on_shows", !T().hidden);

  // Picking a carded issue frames its card between the ticker and the open sheet.
  const pick = k => { T().querySelectorAll(".ab-tk")[keys().indexOf(k)].click(); return new Promise(r => setTimeout(r, 500)); };
  await pick("nightwatch#1");
  const cr = card.getBoundingClientRect(), tr = T().getBoundingClientRect(), sr = SH().getBoundingClientRect();
  check("card_framed_above_sheet", cr.top >= tr.bottom - 1 && cr.bottom <= sr.top + 1, `card ${cr.top}..${cr.bottom} ticker ${tr.bottom} sheet ${sr.top}`);

  // Arrows and + - 0 inside the sheet or the ticker neither pan nor zoom the board.
  const W = root.querySelector('[data-r="world"]'), zoom = () => root.querySelector('[data-r="zv"]').textContent;
  const tf = W.style.transform, z0 = zoom();
  const press = (el, ks) => { el.focus(); for (const k of ks) el.dispatchEvent(new KeyboardEvent("keydown", {key: k, bubbles: true, cancelable: true})); };
  press(SH().querySelector('[data-stab="stack"]'), ["ArrowDown", "ArrowUp", "ArrowLeft", "+", "-", "0"]);
  press(T().querySelector(".ab-tk"), ["ArrowRight", "ArrowDown", "+", "0"]);
  await wait();
  check("keys_stay_in_sheet_and_ticker", W.style.transform === tf && zoom() === z0, `${tf} -> ${W.style.transform}, ${z0} -> ${zoom()}`);

  // The picked issue leaves the list: selection, ping and summary go; the panel is the card's normal one; no other item turns current.
  api.setOverlay(heat([C2, S, D, E]));
  await wait();
  check("gone_issue_clears", !root.querySelector(".ab-card.ab-ping") && !P.querySelector(".ab-isum") && !P.classList.contains("ab-compact")
    && SH().hidden && !T().querySelector(".ab-tk.cur") && getComputedStyle(P.querySelector("h3")).display !== "none", P.className + " " + T().innerHTML.length);

  // The current item is tracked by key: a poll that drops issues above it keeps it current.
  await pick("nightwatch#4");
  const wasAt = keys().indexOf("nightwatch#4");
  api.setOverlay(heat([S, D]));
  await wait();
  const curKey = () => (T().querySelector(".ab-tk.cur") || {dataset: {}}).dataset.key;
  check("current_kept_by_key", wasAt > 0 && keys().join() === "nightwatch#4,nightwatch#2" && curKey() === "nightwatch#4", wasAt + " " + keys().join() + " cur " + curKey());

  // Heat off while a carded issue is picked clears it too.
  api.setOverlay(heat([S, D, A]));
  await wait();
  T().querySelector('[data-tk="flush"]').click();
  await wait();
  await pick("nightwatch#1");
  root.querySelector('[data-r="heat"]').click();
  await wait();
  check("heat_off_clears_issue", !root.querySelector(".ab-card.ab-ping") && !P.querySelector(".ab-isum") && !P.classList.contains("ab-compact"), P.className);
  root.querySelector('[data-r="heat"]').click();
  await wait();
  check("heat_on_no_stale_summary", !P.querySelector(".ab-isum") && !root.querySelector(".ab-card.ab-ping"));

  // After a poll with no issues, the next issues show straight away instead of waiting as "new".
  api.setOverlay(heat([]));
  await wait();
  check("empty_hides", T().hidden);
  api.setOverlay(heat([C, A]));
  await wait();
  check("empty_then_issues_shown", !T().hidden && keys().length === 2 && T().textContent.includes("1 / 2") && !T().querySelector('[data-tk="flush"]'), T().textContent);

  const vpBox = root.querySelector(".ab-vp").getBoundingClientRect(), tb = T().getBoundingClientRect();
  check("fits_width", tb.left >= vpBox.left && tb.right <= vpBox.right + 1 && document.documentElement.scrollWidth <= window.innerWidth + 1,
    `ticker ${tb.left}..${tb.right} vp ${vpBox.left}..${vpBox.right} doc ${document.documentElement.scrollWidth}/${window.innerWidth}`);
  root.dataset.theme = "dark";
  await wait();
  check("dark_theme_panel", getComputedStyle(T()).backgroundColor !== "rgba(0, 0, 0, 0)");
  check("no_script_ran", !window.__xss);
  } catch (e) { out.errors.push(String(e && e.stack || e)); }
  document.getElementById("result").textContent = JSON.stringify(out);
})();
"""


def page(scenario=None):
    css = (BOARD / "board.css").read_text()
    js = (BOARD / "board.js").read_text()
    fixture = (BOARD / "fixtures" / "firebrik-terminal.board.json").read_text()
    return f"""<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><style>{css}
html,body{{margin:0;height:100%}}#board{{height:100vh}}#plain{{height:400px}}</style></head>
<body><div id="board"></div><div id="plain"></div><pre id="result"></pre>
<script>window.FIXTURE={fixture};</script><script>{js}</script><script>{scenario or SCENARIO}</script></body></html>"""


@unittest.skipUnless(CHROME, "no Chrome or Chromium; set HARNESS_TEST_CHROME")
class BoardAgentsDom(unittest.TestCase):
    def run_page(self, width, height, scenario=None):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "board-agents.html"
            path.write_text(page(scenario))
            proc = subprocess.run(
                [
                    CHROME, "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
                    "--disable-extensions", "--disable-sync", "--allow-file-access-from-files",
                    f"--user-data-dir={tmp}/profile", f"--window-size={width},{height}",
                    "--virtual-time-budget=10000", "--dump-dom", path.as_uri(),
                ],
                capture_output=True, text=True, timeout=90,
            )
        start = proc.stdout.find('<pre id="result">')
        self.assertNotEqual(start, -1, proc.stderr[-2000:])
        body = proc.stdout[start + len('<pre id="result">'):proc.stdout.index("</pre>", start)]
        self.assertTrue(body.strip(), "the scenario did not finish: " + proc.stderr[-2000:])
        return json.loads(html.unescape(body))

    def assert_scenario(self, width, height):
        out = self.run_page(width, height)
        self.assertEqual(out["errors"], [])
        failed = {name: why for name, why in out["checks"].items() if why is not True}
        self.assertEqual(failed, {}, f"at {width}x{height}")
        self.assertGreaterEqual(len(out["checks"]), 25)

    def test_desktop(self):
        self.assert_scenario(1280, 900)

    def test_phone(self):
        self.assert_scenario(390, 844)

    def assert_ticker(self, width, height):
        out = self.run_page(width, height, TICKER)
        self.assertEqual(out["errors"], [])
        failed = {name: why for name, why in out["checks"].items() if why is not True}
        self.assertEqual(failed, {}, f"ticker at {width}x{height}")
        self.assertGreaterEqual(len(out["checks"]), 40)

    def test_ticker_desktop(self):
        self.assert_ticker(1280, 900)

    def test_ticker_phone(self):
        self.assert_ticker(390, 844)


if __name__ == "__main__":
    unittest.main()
