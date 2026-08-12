// First-run guided tour of the PPI window. It runs once per browser, the first
// time the viewer is opened, and walks through the controls sitting around the
// plot. `viewer.html?tour=1` replays it.

const SEEN_KEY = "mayaraTourSeen";

// The controls panel slides in over 250ms (`div.myr_controller` in layout.css).
// A step that opens it has to wait for it to come to rest, or the highlight is
// measured while the panel is still moving and lands off the panel.
const PANEL_SETTLE_MS = 300;

// Highlight ring padding around its target, and the gap between ring and
// tooltip.
const RING_PADDING = 6;
const TOOLTIP_GAP = 14;
const TOOLTIP_WIDTH = 300;
const VIEWPORT_MARGIN = 12;

// Whether the controls panel is open. Each step states which it needs, so
// stepping backwards puts the panel back the way that step found it.
let controlsOpen = false;

function setControls(open) {
  const controller = document.getElementById("myr_controller");
  if (controller) controller.classList.toggle("myr_controller_open", open);
  const changed = open !== controlsOpen;
  controlsOpen = open;
  return changed;
}

// A step without a `target` is shown centred on the plot. A step whose target
// is not on the page is skipped, which is how the radar selector step drops
// out for anyone with a single radar. `panel: "open"` means the step needs the
// controls panel open to have something to point at.
const STEPS = [
  {
    title: "This is the PPI plot",
    text:
      "Your boat sits at the centre of the plot and the bow points up the " +
      "screen. As the antenna turns, the radar paints what it sees — land, " +
      "buoys, other vessels — around you. Here is a quick tour of the " +
      "controls around the plot.",
  },
  {
    target: "#myr_power_lozenge .myr_power_lozenge_button",
    title: "Standby and transmit",
    text:
      "The power button switches the radar between standby and transmit. It " +
      "shows yellow on standby, and green once the antenna is turning and " +
      "transmitting.",
  },
  {
    target: "#myr_power_lozenge_select",
    title: "Choosing a radar",
    text:
      "More than one radar is connected. The radar name next to the power " +
      "button opens the list — pick another one to show it in this window.",
  },
  {
    target: "#myr_range_lozenge",
    title: "Range",
    text:
      "These buttons set how far the radar looks. + steps down to a shorter " +
      "range for more detail close in, − steps up to see further out. The " +
      "range in use is shown between them.",
  },
  {
    target: "#myr_hamburger_button",
    title: "The controls menu",
    text:
      "This button opens the radar controls — gain, sea and rain clutter, " +
      "and everything else your radar supports.",
  },
  {
    target: "#myr_controller",
    panel: "open",
    title: "The settings",
    text:
      "The settings are grouped into sections: the everyday ones at the top, " +
      "with target, trail, advanced and installation settings below. Scroll " +
      "down for the rest.",
  },
  {
    target: "#myr_close_controls",
    panel: "open",
    title: "Closing the menu",
    text:
      "Close the controls again with this button. The radar keeps running " +
      "and the plot stays live behind the panel.",
  },
];

let steps = [];
let stepIndex = 0;
let elements = null;
let settleTimer = null;

function hasSeenTour() {
  try {
    return localStorage.getItem(SEEN_KEY) === "true";
  } catch {
    // localStorage unavailable (private mode) — treat the tour as unseen.
    return false;
  }
}

function markTourSeen() {
  try {
    localStorage.setItem(SEEN_KEY, "true");
  } catch {
    // ignore quota errors — the tour then reappears next visit.
  }
}

function buildOverlay() {
  const root = document.createElement("div");
  root.className = "myr_tour";

  const masks = [];
  for (let i = 0; i < 4; i++) {
    const mask = document.createElement("div");
    mask.className = "myr_tour_mask";
    root.appendChild(mask);
    masks.push(mask);
  }

  // The ring covers its target, so clicks during the tour reach the ring
  // instead of the control underneath: nobody starts transmitting by
  // following along. Clicking it moves on, so the click is not simply lost.
  const ring = document.createElement("div");
  ring.className = "myr_tour_ring";
  ring.addEventListener("click", next);
  root.appendChild(ring);

  const tip = document.createElement("div");
  tip.className = "myr_tour_tip";

  const title = document.createElement("div");
  title.className = "myr_tour_tip_title";

  const text = document.createElement("div");
  text.className = "myr_tour_tip_text";

  const footer = document.createElement("div");
  footer.className = "myr_tour_tip_footer";

  const progress = document.createElement("span");
  progress.className = "myr_tour_progress";

  const skipBtn = document.createElement("button");
  skipBtn.type = "button";
  skipBtn.className = "myr_tour_button myr_tour_skip";
  skipBtn.textContent = "Skip";
  skipBtn.addEventListener("click", end);

  const backBtn = document.createElement("button");
  backBtn.type = "button";
  backBtn.className = "myr_tour_button";
  backBtn.textContent = "Back";
  backBtn.addEventListener("click", back);

  const nextBtn = document.createElement("button");
  nextBtn.type = "button";
  nextBtn.className = "myr_tour_button myr_tour_next";
  nextBtn.addEventListener("click", next);

  footer.appendChild(progress);
  footer.appendChild(skipBtn);
  footer.appendChild(backBtn);
  footer.appendChild(nextBtn);

  tip.appendChild(title);
  tip.appendChild(text);
  tip.appendChild(footer);
  root.appendChild(tip);

  document.body.appendChild(root);

  return { root, masks, ring, tip, title, text, progress, backBtn, nextBtn };
}

// Cover the whole viewport except `rect`, using one mask above, one below and
// one on either side of it. A zero-size rect leaves the viewport fully
// covered, which is what the opening step wants.
function positionMasks(rect) {
  const [top, bottom, left, right] = elements.masks;
  const w = window.innerWidth;
  const h = window.innerHeight;

  top.style.cssText = `left:0;top:0;width:${w}px;height:${Math.max(0, rect.top)}px`;
  bottom.style.cssText = `left:0;top:${rect.bottom}px;width:${w}px;height:${Math.max(0, h - rect.bottom)}px`;
  left.style.cssText = `left:0;top:${rect.top}px;width:${Math.max(0, rect.left)}px;height:${Math.max(0, rect.height)}px`;
  right.style.cssText = `left:${rect.right}px;top:${rect.top}px;width:${Math.max(0, w - rect.right)}px;height:${Math.max(0, rect.height)}px`;
}

// Put the tooltip below the highlight, or above / beside it when there is no
// room, so it never covers what it is describing.
function positionTooltip(rect, hasTarget) {
  const tip = elements.tip;
  tip.style.width = `${TOOLTIP_WIDTH}px`;

  const w = window.innerWidth;
  const h = window.innerHeight;
  const tipHeight = tip.offsetHeight;

  if (!hasTarget) {
    tip.style.left = `${Math.round((w - TOOLTIP_WIDTH) / 2)}px`;
    tip.style.top = `${Math.round((h - tipHeight) / 2)}px`;
    return;
  }

  const clamp = (value, max) =>
    Math.round(Math.min(Math.max(value, VIEWPORT_MARGIN), Math.max(VIEWPORT_MARGIN, max)));

  const below = rect.bottom + TOOLTIP_GAP;
  const above = rect.top - TOOLTIP_GAP - tipHeight;
  const beside = clamp(rect.top, h - tipHeight - VIEWPORT_MARGIN);

  if (below + tipHeight + VIEWPORT_MARGIN <= h) {
    tip.style.top = `${Math.round(below)}px`;
    tip.style.left = `${clamp(rect.left, w - TOOLTIP_WIDTH - VIEWPORT_MARGIN)}px`;
  } else if (above >= VIEWPORT_MARGIN) {
    tip.style.top = `${Math.round(above)}px`;
    tip.style.left = `${clamp(rect.left, w - TOOLTIP_WIDTH - VIEWPORT_MARGIN)}px`;
  } else if (rect.left - TOOLTIP_GAP - TOOLTIP_WIDTH >= VIEWPORT_MARGIN) {
    tip.style.top = `${beside}px`;
    tip.style.left = `${Math.round(rect.left - TOOLTIP_GAP - TOOLTIP_WIDTH)}px`;
  } else {
    tip.style.top = `${beside}px`;
    tip.style.left = `${clamp(rect.right + TOOLTIP_GAP, w - TOOLTIP_WIDTH - VIEWPORT_MARGIN)}px`;
  }
}

function targetRect(step) {
  if (!step.target) return null;
  const el = document.querySelector(step.target);
  if (!el) return null;

  const box = el.getBoundingClientRect();
  return {
    top: box.top - RING_PADDING,
    left: box.left - RING_PADDING,
    right: box.right + RING_PADDING,
    bottom: box.bottom + RING_PADDING,
    width: box.width + 2 * RING_PADDING,
    height: box.height + 2 * RING_PADDING,
  };
}

function layoutStep() {
  const step = steps[stepIndex];
  const rect = targetRect(step);

  if (rect) {
    elements.ring.style.display = "block";
    elements.ring.style.left = `${Math.round(rect.left)}px`;
    elements.ring.style.top = `${Math.round(rect.top)}px`;
    elements.ring.style.width = `${Math.round(rect.width)}px`;
    elements.ring.style.height = `${Math.round(rect.height)}px`;
    positionMasks(rect);
  } else {
    elements.ring.style.display = "none";
    const centre = window.innerWidth / 2;
    const middle = window.innerHeight / 2;
    positionMasks({
      top: middle,
      bottom: middle,
      left: centre,
      right: centre,
      width: 0,
      height: 0,
    });
  }

  positionTooltip(rect, rect !== null);
}

function showStep() {
  const step = steps[stepIndex];
  const panelMoved = setControls(step.panel === "open");

  elements.title.textContent = step.title;
  elements.text.textContent = step.text;
  elements.progress.textContent = `Step ${stepIndex + 1} of ${steps.length}`;
  elements.backBtn.disabled = stepIndex === 0;
  elements.nextBtn.textContent =
    stepIndex === steps.length - 1 ? "Done" : "Next";

  clearTimeout(settleTimer);
  if (panelMoved) {
    settleTimer = setTimeout(layoutStep, PANEL_SETTLE_MS);
  } else {
    layoutStep();
  }
}

function next() {
  if (stepIndex >= steps.length - 1) {
    end();
    return;
  }
  stepIndex++;
  showStep();
}

function back() {
  if (stepIndex === 0) return;
  stepIndex--;
  showStep();
}

function onKeyDown(event) {
  if (event.key === "Escape") {
    end();
  } else if (event.key === "ArrowRight" || event.key === "Enter") {
    next();
  } else if (event.key === "ArrowLeft") {
    back();
  } else {
    return;
  }
  event.preventDefault();
  event.stopPropagation();
}

function end() {
  clearTimeout(settleTimer);
  window.removeEventListener("resize", layoutStep);
  document.removeEventListener("keydown", onKeyDown, true);

  if (elements) {
    elements.root.remove();
    elements = null;
  }

  // The tour opens the controls panel to explain it; leave the viewer the way
  // it was found.
  setControls(false);
  markTourSeen();
}

// Start the tour if this browser has not seen it yet. `?tour=1` forces it.
export function startTourIfFirstVisit() {
  const forced =
    new URLSearchParams(window.location.search).get("tour") === "1";
  if (!forced && hasSeenTour()) return;

  // An embedded viewer is one pane of a larger page; a tour per pane would
  // fight for the same screen.
  if (window.self !== window.top) return;

  steps = STEPS.filter((step) => !step.target || document.querySelector(step.target));
  if (steps.length === 0) return;

  stepIndex = 0;
  elements = buildOverlay();
  document.addEventListener("keydown", onKeyDown, true);
  window.addEventListener("resize", layoutStep);
  showStep();
}
