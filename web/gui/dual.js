// Side-by-side view for the two ranges of a dual-range antenna. Each pane is
// the normal viewer in an iframe, so controls, rendering and streams behave
// exactly as in a single-radar window.

import { fetchRadars } from "./api.js";

function fail(container, message) {
  container.setAttribute("role", "alert");
  container.className = "myr_dual_error";
  container.textContent = message;
}

window.onload = async function () {
  const params = new URLSearchParams(window.location.search);
  const container = document.getElementById("myr_dual");

  const ids = ["a", "b"].map((k) => params.get(k)?.trim()).filter(Boolean);
  if (ids.length !== 2 || ids[0] === ids[1]) {
    fail(
      container,
      "Dual view needs two distinct radar ids: dual.html?a=<id>&b=<id>"
    );
    return;
  }

  // The ids arrive from the URL, so they may be stale or hand-edited. Without
  // checking them against the server, two unrelated radars — or ids that no
  // longer exist — would load as panes and sit retrying failed capability
  // requests, with nothing on screen to explain why.
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

  // dualGroup is only set for dual-range radars, and is the shared key of the
  // physical antenna — so equal non-empty groups is exactly "these two are the
  // ranges of one antenna".
  const [groupA, groupB] = ids.map((id) => radars[id].dualGroup);
  if (!groupA || groupA !== groupB) {
    fail(
      container,
      `${ids[0]} and ${ids[1]} are not two ranges of the same antenna`
    );
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
