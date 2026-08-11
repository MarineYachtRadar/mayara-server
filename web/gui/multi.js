// Several radars shown at once. Each pane is the normal viewer in an iframe,
// so controls, rendering and streams behave exactly as in a single-radar
// window.

import { fetchRadars } from "./api.js";

function fail(container, message) {
  container.setAttribute("role", "alert");
  container.className = "myr_multi_error";
  container.textContent = message;
}

window.onload = async function () {
  const params = new URLSearchParams(window.location.search);
  const container = document.getElementById("myr_multi");

  const ids = (params.get("ids") || "")
    .split(",")
    .map((id) => id.trim())
    .filter(Boolean);

  if (ids.length < 2 || new Set(ids).size !== ids.length) {
    fail(
      container,
      "Combined view needs two or more distinct radar ids: multi.html?ids=<id>,<id>"
    );
    return;
  }

  // The ids arrive from the URL, so they may be stale or hand-edited. Without
  // checking them against the server, ids that no longer exist would load as
  // panes and sit retrying failed capability requests, with nothing on screen
  // to explain why.
  let radars;
  try {
    radars = await fetchRadars();
  } catch (e) {
    fail(container, `Could not load the radar list: ${e.message}`);
    return;
  }

  const missing = ids.filter((id) => !radars[id]);
  if (missing.length) {
    fail(container, `Unknown radar id: ${missing.join(", ")}`);
    return;
  }

  container.dataset.panes = String(ids.length);
  for (const id of ids) {
    const pane = document.createElement("iframe");
    pane.className = "myr_multi_pane";
    // `pane` tells the viewer it is part of this layout, so picking another
    // combined view from its radar selector replaces the page instead of
    // nesting a second layout inside one pane.
    pane.src = "viewer.html?id=" + encodeURIComponent(id) + "&pane=1";
    pane.title = radars[id].name || id;
    container.appendChild(pane);
  }
};
