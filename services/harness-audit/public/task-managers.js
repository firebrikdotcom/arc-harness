// Task Managers pages: progressive touches only. Every link and form works without this file.
(() => {
  const page = document.querySelector(".tm-page");
  if (!page) return;

  // A table row opens its item, unless the click was on a link or a control inside it.
  page.addEventListener("click", (event) => {
    if (event.target.closest("a,button,input,select,textarea,label")) return;
    const row = event.target.closest("tr[data-href]");
    if (row && row.dataset.href.startsWith("/tasks/")) location.assign(row.dataset.href);
  });

  // Forms that change something upstream ask first.
  page.addEventListener("submit", (event) => {
    const question = event.target.dataset && event.target.dataset.tmConfirm;
    if (question && !window.confirm(question)) event.preventDefault();
  });

  // On an item: arrows move to the previous or next item, Escape returns to the list.
  if (page.hasAttribute("data-tm-item")) {
    document.addEventListener("keydown", (event) => {
      if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
      if (event.target.closest("input,select,textarea,[contenteditable]")) return;
      if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
        const link = page.querySelector(`[data-tm-nav="${event.key === "ArrowLeft" ? "prev" : "next"}"]`);
        if (link) link.click();
      } else if (event.key === "Escape" && page.dataset.tmUp) {
        location.assign(page.dataset.tmUp);
      }
    });
  }

  // A run that is working reloads to show its progress. A run waiting for you never does.
  const seconds = Number(page.dataset.tmRefresh);
  if (seconds > 0) {
    setTimeout(() => { if (!document.hidden) location.reload(); else document.addEventListener("visibilitychange", () => location.reload(), { once: true }); }, seconds * 1000);
  }
})();
