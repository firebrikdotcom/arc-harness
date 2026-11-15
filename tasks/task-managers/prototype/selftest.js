// Drives the prototype through a full delegation; result goes into <body data-selftest>.
(async () => {
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const out = [];
  const ff = () => { for (const r of store.runs) r.phaseStarted = 0; tick(); };
  try {
    localStorage.clear();
    store = { runs: [], repoMap: {}, seq: 0 };
    const t = todos[0];
    go(`#/t/${t.id}`); await wait(50);
    document.querySelector(`[data-action="delegate"]`).click();
    out.push("no-repo-blocked=" + (store.runs.length === 0));
    document.getElementById("f-repo").value = "firebrikcom";
    document.querySelector(`[data-action="delegate"]`).click();
    const run = store.runs[0];
    out.push("created=" + !!run);
    for (let i = 0; i < 3; i++) ff();
    out.push("at=" + phaseOf(run).key);
    document.querySelector(`[data-action="approve"]`).click();
    for (let i = 0; i < 6; i++) ff();
    out.push("at=" + phaseOf(run).key + " pr=" + (run.pr ? run.pr.number : "none"));
    document.querySelector(`[data-action="approve"]`).click();
    out.push("final=" + statusInfo(run).label);
    go("#/runs"); await wait(50);
    out.push("runs-rows=" + document.querySelectorAll("tr[data-href^='#/runs/']").length);
    // bulk
    go(`#/p/${projects[0].id}`); await wait(50);
    for (let i = 0; i < 2; i++) { document.querySelectorAll("[data-select]:not([disabled]):not(:checked)")[0].click(); await wait(10); }
    document.querySelector(`[data-action="bulk-delegate"]`).click();
    const br = document.getElementById("b-repo"); if (br) br.value = "arc";
    document.querySelector(`[data-action="bulk-confirm"]`).click();
    out.push("bulk-total=" + store.runs.length);
    go(`#/t/${todos[1].id}`); await wait(50);
    out.push("pager=" + document.querySelector(".tm-pager .tm-count").textContent.split(" ·")[0]);
  } catch (e) { out.push("ERROR " + e.message); }
  document.body.dataset.selftest = out.join(" | ");
})();
