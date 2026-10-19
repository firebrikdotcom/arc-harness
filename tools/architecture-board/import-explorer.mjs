// Convert a hand-built architecture-explorer.html (constants FRAMES/NODES/EDGES/FLOWS) into board.json.
// Reads the source file only; writes the board to the path given (never into the source project).
//   node import-explorer.mjs SOURCE.html OUT.json
import {readFileSync, writeFileSync} from "node:fs";
import vm from "node:vm";
import {basename} from "node:path";

const [src, out] = process.argv.slice(2);
if (!src || !out) { console.error("usage: import-explorer.mjs SOURCE.html OUT.json"); process.exit(2); }
const html = readFileSync(src, "utf8");
const start = html.indexOf("const REPO=");
const end = html.indexOf("const $=s=>");
if (start < 0 || end < 0) { console.error("explorer constants not found"); process.exit(1); }
const code = html.slice(start, end).replace(/^const /gm, "var ");
const ctx = {};
vm.runInNewContext(code, ctx);

const TONE = {"--phone": "orange", "--app": "blue", "--core": "green", "--term": "teal", "--remote": "red", "--platform": "purple", "--google": "gray", "--build": "slate"};
const KTONE = {call: ["slate", "solid"], remote: ["red", "dashed"], platform: ["purple", "dotted"], build: ["gray", "faint"]};
const title = (html.match(/<h1>([^<]+)</) || [, "Project"])[1].trim();
const repo = (ctx.REPO || "").replace(/\/blob\/([^/]+)\/$/, "");
const branch = ((ctx.REPO || "").match(/\/blob\/([^/]+)\//) || [, "main"])[1];

const board = {
  version: 1,
  title,
  repo_url: repo,
  default_branch: branch,
  source: "imported from " + basename(src),  // never an absolute path: tracked files stay free of personal paths
  overview: {heading: "The app at a glance", text: "The phone talks directly to your own machine over SSH. Herdr runs there and owns workspaces, panes and agents; the app mirrors it and forwards your input."},
  edge_kinds: Object.fromEntries(Object.entries(ctx.KINDS).map(([k, v]) => [k, {label: v.label, tone: KTONE[k]?.[0] || "slate", style: KTONE[k]?.[1] || "solid"}])),
  frames: ctx.FRAMES.map(f => ({id: f.id, title: f.title, tag: f.tag, tone: TONE[f.c] || "slate", col: f.col, row: f.row, cols: f.cols, rows: f.rows, external: !!f.dashed})),
  nodes: ctx.NODES.map(n => ({
    id: n.id, frame: n.f, col: n.col, row: n.row, icon: n.icon, title: n.title, sub: n.sub,
    summary: n.summary, responsibility: n.resp, external: !!n.external,
    files: n.files.map(([path, note]) => ({path, note})),
    tests: n.tests,
    owns: [...new Set([...n.files.map(([p]) => p), ...n.tests])],
  })),
  edges: ctx.EDGES.map(({from, to, label, kind}) => ({from, to, label, kind})),
  flows: ctx.FLOWS.map(({id, title, sub, steps, text}) => ({id, title, sub, steps, text})),
};
writeFileSync(out, JSON.stringify(board, null, 1) + "\n");
console.log(`${out}: ${board.frames.length} frames, ${board.nodes.length} nodes, ${board.edges.length} edges, ${board.flows.length} flows`);
