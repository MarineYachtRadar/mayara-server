// Data boxes for the most interesting targets: ARPA targets stacked down the
// right edge of the PPI, AIS vessels down the left. The PPI is told how much
// of each edge they cover so it can move clear of them.

export { TargetDataBoxes };

import { rankInteresting } from "./target-summary.js";

// Boxes per side. More than this is a wall of numbers nobody reads at sea.
const MAX_DATA_BOXES = 5;

const KNOTS_PER_MS = 3600 / 1852;
const NO_VALUE = "—";

function formatBearing(rad) {
  if (rad == null) return NO_VALUE;
  const deg = Math.round((((rad * 180) / Math.PI) % 360 + 360) % 360) % 360;
  return String(deg).padStart(3, "0") + "°";
}

function formatSpeed(ms) {
  return ms == null ? NO_VALUE : (ms * KNOTS_PER_MS).toFixed(1) + " kn";
}

function formatTcpa(seconds) {
  if (seconds == null) return NO_VALUE;
  if (seconds < 0) return "passed";
  const total = Math.round(seconds);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

class TargetDataBoxes {
  /**
   * @param {PPI} ppi
   * @param {HTMLElement} container - the PPI area the boxes overlay
   */
  constructor(ppi, container) {
    this.ppi = ppi;
    this.container = container;
    this.left = this.#column("myr_data_boxes_left");
    this.right = this.#column("myr_data_boxes_right");
    this.renderPending = false;

    ppi.onTrackedChange = () => this.#scheduleRender();
    // A breakpoint hiding the columns, or a viewport too short for all the
    // boxes, changes the space they take.
    const observer = new ResizeObserver(() => this.#scheduleRender());
    observer.observe(this.left);
    observer.observe(this.right);
  }

  #column(sideClass) {
    const column = document.createElement("div");
    column.className = `myr_data_boxes ${sideClass}`;
    this.container.appendChild(column);
    return column;
  }

  #scheduleRender() {
    if (this.renderPending) return;
    this.renderPending = true;
    requestAnimationFrame(() => {
      this.renderPending = false;
      this.#render();
    });
  }

  #render() {
    const { arpa, ais } = this.ppi.getTrackedTargets();
    const maxRange = this.ppi.range > 0 ? this.ppi.range : Infinity;
    this.#fill(this.right, rankInteresting(arpa, MAX_DATA_BOXES, maxRange), "myr_data_box_arpa");
    this.#fill(this.left, rankInteresting(ais, MAX_DATA_BOXES, maxRange), "myr_data_box_ais");
    this.ppi.setSideInsets(this.#leftInset(), this.#rightInset());
  }

  #fill(column, targets, kindClass) {
    column.replaceChildren(...targets.map((t) => this.#box(t, kindClass)));
    // Drop the boxes the column has no room for rather than show half a one
    while (column.lastElementChild && column.lastElementChild.offsetTop + column.lastElementChild.offsetHeight > column.clientHeight) {
      column.lastElementChild.remove();
    }
  }

  #box(target, kindClass) {
    const box = document.createElement("div");
    box.className = `myr_data_box ${kindClass}`;
    if (target.danger) box.classList.add("myr_data_box_danger");
    if (target.status) box.classList.add(`myr_data_box_${target.status}`);

    const header = document.createElement("div");
    header.className = "myr_data_box_header";
    const label = document.createElement("span");
    label.className = "myr_data_box_label";
    label.textContent = target.label;
    label.title = target.label;
    header.appendChild(label);
    if (target.status) {
      const status = document.createElement("span");
      status.className = "myr_data_box_status";
      status.textContent = target.status;
      header.appendChild(status);
    }
    box.appendChild(header);

    const fields = [
      ["BRG", formatBearing(target.bearing)],
      ["RNG", this.ppi.formatDistance(target.range)],
      ["COG", formatBearing(target.course)],
      ["SOG", formatSpeed(target.speed)],
      ["CPA", target.cpa == null ? NO_VALUE : this.ppi.formatDistance(target.cpa)],
      ["TCPA", formatTcpa(target.tcpa)],
    ];
    const grid = document.createElement("dl");
    grid.className = "myr_data_box_fields";
    for (const [name, value] of fields) {
      const dt = document.createElement("dt");
      dt.textContent = name;
      const dd = document.createElement("dd");
      dd.textContent = value;
      grid.append(dt, dd);
    }
    box.appendChild(grid);
    return box;
  }

  // Canvas width covered from each edge to the far side of its column; zero
  // while the column is empty or hidden by a breakpoint.
  #leftInset() {
    if (!this.left.childElementCount || !this.left.offsetWidth) return 0;
    return Math.ceil(this.left.getBoundingClientRect().right - this.container.getBoundingClientRect().left);
  }

  #rightInset() {
    if (!this.right.childElementCount || !this.right.offsetWidth) return 0;
    return Math.ceil(this.container.getBoundingClientRect().right - this.right.getBoundingClientRect().left);
  }
}
