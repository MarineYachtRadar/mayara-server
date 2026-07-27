// Side-by-side view for the two ranges of a dual-range antenna. Each pane is
// the normal viewer in an iframe, so controls, rendering and streams behave
// exactly as in a single-radar window.

window.onload = function () {
  const params = new URLSearchParams(window.location.search);
  const container = document.getElementById("myr_dual");

  const ids = ["a", "b"].map((k) => params.get(k)?.trim()).filter(Boolean);
  if (ids.length !== 2 || ids[0] === ids[1]) {
    container.setAttribute("role", "alert");
    container.className = "myr_dual_error";
    container.textContent =
      "Dual view needs two distinct radar ids: dual.html?a=<id>&b=<id>";
    return;
  }

  for (const id of ids) {
    const pane = document.createElement("iframe");
    pane.className = "myr_dual_pane";
    pane.src = "viewer.html?id=" + encodeURIComponent(id);
    pane.title = "Radar " + id;
    container.appendChild(pane);
  }
};
