"""Session focus on the architecture board, driven in headless Chrome.

The page inlines board.css, board.js and the fixture board, mounts one board
with `session: true` and feeds it focus documents like the dashboard's
focus.json, then mounts a second plain board and checks it is untouched.
The scenario prints its findings as JSON into the DOM that Chrome dumps.
Nothing reaches a server or the person's Chrome profile.
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
  const out = {checks: {}, errors: []};
  const check = (name, ok, detail) => { out.checks[name] = ok ? true : String(detail === undefined ? "failed" : detail); };
  const wait = () => new Promise(r => setTimeout(r, 40));
  try {
  const root = document.getElementById("board");
  const cards = () => [...root.querySelectorAll(".ab-card")];
  const ids = () => cards().map(c => c.dataset.id).sort().join(",");
  const card = id => root.querySelector(`.ab-card[data-id="${id}"]`);
  const badge = id => (card(id).querySelector(".ab-badges") || {textContent: ""}).textContent;
  const press = mode => { root.querySelector(`[data-mode="${mode}"]`).click(); };
  const doc = (extra) => Object.assign({
    target: "wt-1", board_target: "repo-1", parent: "repo-1",
    session: {key: "K".repeat(32), owner: {user: "savior", login: null}, branch: "eng-1-focus", base: "develop",
      root: "/work/repo.worktrees/eng-1-focus", run_id: "r1", phase: "build", title: "Fix <img src=x onerror=window.__xss=1> terminal", live: true, last_active: "2026-11-05T10:00:00Z"},
    signals: {
      terminal: {edited: ["app/src/main/java/com/firebrik/terminal/terminal/TerminalBridge.kt", "app/src/test/Bridge.kt"], scope: ["app/src/main/java/com/firebrik/terminal/terminal/TerminalBridge.kt"], touched: []},
      engine: {scope: ["terminal/src/main/java/com/termux/terminal/TerminalEmulator.java"]},
      keys: {touched: ["core/src/main/kotlin/com/firebrik/terminal/core/keys/TerminalKeys.kt"]},
    },
    unmapped: ["docs/<b>notes</b>.md"], summary: "It changed the terminal bridge and planned an engine change.",
  }, extra || {});

  const api = window.ArchitectureBoard.mount(root, window.FIXTURE, {session: true, target: "repo-1"});
  check("api_exposes_focus", typeof api.setFocus === "function" && typeof api.setMode === "function");
  api.setFocus(doc());
  await wait();

  // Focus mode: only the session's cards, packed into a compact grid.
  check("focus_cards_only", ids() === "engine,keys,terminal", ids());
  const lefts = cards().map(c => parseInt(c.style.left, 10)), tops = cards().map(c => parseInt(c.style.top, 10));
  check("compact_columns", new Set(lefts).size === 2 && Math.max(...lefts) <= 320, lefts.join());
  check("compact_rows", new Set(tops).size === 2 && Math.max(...tops) - Math.min(...tops) <= 204, tops.join());
  check("frames_only_for_shown", root.querySelectorAll(".ab-frame").length === 3, root.querySelectorAll(".ab-frame").length);
  check("pill_changed", badge("terminal").includes("2 changed"), badge("terminal"));
  check("pill_planned", badge("engine").includes("planned") && !badge("engine").includes("changed"), badge("engine"));
  check("pill_read", badge("keys").includes("read"), badge("keys"));
  check("card_levels", card("terminal").classList.contains("lv-focus") && card("engine").classList.contains("scope-only") && card("keys").classList.contains("touched-only"));
  const rest = root.querySelector('[data-r="rest"]');
  check("folded_line", !rest.hidden && rest.textContent.includes("17 other parts of the system are folded away") && !!rest.querySelector("button"), rest.textContent);
  check("hot_edges_only_between_focus", root.querySelectorAll("path.edge.hot").length === 2 && root.querySelectorAll("path.edge").length === 2, root.querySelectorAll("path.edge").length);
  check("focus_button_pressed", root.querySelector('[data-mode="focus"]').getAttribute("aria-pressed") === "true");

  // Side panel: facts, plain summary, legend, unmapped files; escaped.
  const panel = root.querySelector(".ab-panel");
  check("panel_facts", panel.textContent.includes("eng-1-focus") && panel.textContent.includes("develop") && panel.textContent.includes("/work/repo.worktrees/eng-1-focus"), panel.textContent.slice(0, 300));
  check("panel_phase", !!panel.querySelector(".ab-phase span.on") && panel.querySelector(".ab-phase span.on").textContent === "build");
  check("panel_summary", panel.textContent.includes("It changed the terminal bridge"));
  check("panel_legend", panel.textContent.includes("Changed in this worktree") && panel.textContent.includes("Neighbor, shown for context"));
  check("panel_unmapped_escaped", panel.textContent.includes("docs/<b>notes</b>.md") && ![...panel.querySelectorAll("b")].some(b => b.textContent === "notes"));
  check("title_escaped", !panel.querySelector("img") && !window.__xss && panel.textContent.includes("<img src=x"));

  // Why this card.
  card("terminal").dispatchEvent(new MouseEvent("click", {bubbles: true, detail: 0}));
  await wait();
  check("why_changed_files", panel.textContent.includes("Why this card") && panel.textContent.includes("TerminalBridge.kt") && panel.textContent.includes("Bridge.kt") && panel.textContent.includes("changed"), panel.textContent.slice(0, 400));
  check("why_planned", panel.textContent.includes("planned in task"));
  card("keys").dispatchEvent(new MouseEvent("click", {bubbles: true, detail: 0}));
  await wait();
  check("why_read_only", panel.textContent.includes("read only") && panel.textContent.includes("TerminalKeys.kt") && !panel.textContent.includes("planned in task"));

  // Neighbours: off by default, only of changed or planned cards.
  const nb = root.querySelector('[data-r="nb"]');
  check("neighbors_off_by_default", nb.checked === false && !root.querySelector(".lv-ctx"));
  nb.checked = true; nb.dispatchEvent(new Event("change", {bubbles: true}));
  await wait();
  check("neighbors_shown", ids() === "app,compose,engine,keys,shell,terminal", ids());
  check("neighbors_dashed", ["app", "compose", "shell"].every(i => card(i).classList.contains("lv-ctx") && badge(i).includes("neighbor")), root.querySelectorAll(".lv-ctx").length);
  nb.checked = false; nb.dispatchEvent(new Event("change", {bubbles: true}));
  await wait();
  check("neighbors_off_again", ids() === "engine,keys,terminal");

  // Highlight on the full map: everything drawn, untouched cards and edges dimmed.
  rest.querySelector("button").click();
  await wait();
  check("show_dimmed_switches_mode", root.querySelector('[data-mode="highlight"]').getAttribute("aria-pressed") === "true" && cards().length === 20, cards().length);
  check("highlight_dims_rest", root.querySelectorAll(".ab-card.lv-rest").length === 17 && !card("terminal").classList.contains("lv-rest"));
  check("highlight_dims_edges", root.querySelectorAll("path.edge.faded").length > 15 && root.querySelectorAll("path.edge.hot").length === 2, root.querySelectorAll("path.edge.faded").length);
  check("highlight_original_layout", card("user").style.left === "0px" && card("herdr").style.left === (4 * 320) + "px", card("herdr").style.left);
  check("folded_line_hidden", rest.hidden);

  // Whole system: as a plain board.
  press("full");
  await wait();
  check("full_mode_plain", cards().length === 20 && !root.querySelector(".lv-focus,.lv-rest,.lv-ctx") && !root.querySelector(".ab-card .ab-badge") && !root.querySelector("path.edge.hot,path.edge.faded"));

  // Picking a card that focus mode folded away opens the full map.
  press("focus");
  await wait();
  check("back_to_focus", ids() === "engine,keys,terminal");
  api.select("android");
  await wait();
  check("select_hidden_card_unfolds", cards().length === 20 && root.querySelector('[data-mode="highlight"]').getAttribute("aria-pressed") === "true" && card("android").classList.contains("sel"));
  api.setMode("focus");
  await wait();

  // A refresh with the same document leaves the DOM alone; a changed one redraws.
  const before = card("terminal");
  api.setFocus(doc());
  await wait();
  check("same_focus_keeps_dom", card("terminal") === before);
  api.setFocus(doc({signals: Object.assign({}, doc().signals, {notify: {edited: ["app/src/main/java/com/firebrik/terminal/notify/AgentNotifier.kt"]}})}));
  await wait();
  check("changed_focus_adds_card", ids() === "engine,keys,notify,terminal", ids());

  // An empty session says so instead of drawing nothing.
  api.setFocus(doc({signals: {}, unmapped: []}));
  await wait();
  check("empty_session_message", cards().length === 0 && !!root.querySelector(".ab-empty") && root.querySelector(".ab-empty").textContent.includes("Nothing on the map yet"));
  api.setFocus(doc());
  await wait();
  check("empty_message_removed", !root.querySelector(".ab-empty") && cards().length === 3);

  // The page does not scroll sideways at this width.
  check("no_horizontal_scroll", document.documentElement.scrollWidth <= window.innerWidth + 1, document.documentElement.scrollWidth + "/" + window.innerWidth);
  const seg = root.querySelector(".ab-seg").getBoundingClientRect();
  check("modes_inside_viewport", seg.left >= 0 && seg.right <= window.innerWidth + 1, seg.left + ".." + seg.right);

  // A plain project board renders exactly as before.
  history.replaceState(null, "", location.pathname);
  const plainRoot = document.getElementById("plain");
  const plainApi = window.ArchitectureBoard.mount(plainRoot, window.FIXTURE, {});
  const plain = [...plainRoot.querySelectorAll(".ab-card")];
  check("plain_all_cards", plain.length === 20 && plainRoot.querySelectorAll(".ab-frame").length === 8 && plainRoot.querySelectorAll("path.edge").length === 28);
  check("plain_no_session_ui", !plainRoot.querySelector(".ab-sess,.ab-rest,.ab-seg,.ab-card .ab-badge,.lv-focus,.lv-rest,path.edge.hot,path.edge.faded"));
  check("plain_original_layout", plain.every(c => { const n = window.FIXTURE.nodes.find(x => x.id === c.dataset.id); return c.style.left === (n.col * 320) + "px" && c.style.top === (n.row * 204 + 46) + "px"; }));
  check("plain_panel_is_overview", plainRoot.querySelector(".ab-panel").textContent.includes("How to read this board") && !plainRoot.querySelector(".ab-panel").textContent.includes("Task in a worktree"));
  check("plain_frame_chips_visible", [...plainRoot.querySelectorAll(".ab-chip")].every(c => !c.hidden));
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
class BoardSessionDom(unittest.TestCase):
    def run_page(self, width, height):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "board-session.html"
            path.write_text(page())
            proc = subprocess.run(
                [
                    CHROME, "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
                    *(["--no-sandbox"] if os.environ.get("CI") else []),
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
        self.assertGreaterEqual(len(out["checks"]), 40)

    def test_desktop(self):
        self.assert_scenario(1280, 900)

    def test_phone(self):
        self.assert_scenario(390, 844)


if __name__ == "__main__":
    unittest.main()
