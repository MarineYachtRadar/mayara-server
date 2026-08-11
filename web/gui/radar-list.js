// How the radar list is presented across the GUI: the overview page and the
// viewer's radar selector offer the same radars, in the same order, with the
// same combined views.

/**
 * The combined views offered for a radar list: consecutive pairs — which is
 * where the two ranges of one antenna land in the order the API lists them —
 * and, from three radars up, every radar at once.
 * @param {Array<{id: string, name: string}>} radars in the API's order
 * @returns {Array<{label: string, ids: string[]}>}
 */
export function radarCombinations(radars) {
  const combinations = [];
  for (let i = 0; i + 1 < radars.length; i += 2) {
    const pair = radars.slice(i, i + 2);
    combinations.push({
      label: pair.map((radar) => radar.name).join(" + "),
      ids: pair.map((radar) => radar.id),
    });
  }
  if (radars.length > 2) {
    combinations.push({
      label: `Show ${radars.length} radars`,
      ids: radars.map((radar) => radar.id),
    });
  }
  return combinations;
}

/** Link to the combined view of `ids`, in that pane order. */
export function multiViewUrl(ids) {
  return "multi.html?ids=" + ids.map(encodeURIComponent).join(",");
}
