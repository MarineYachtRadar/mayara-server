export { SpokeProcessorFactory, SpokeProcessingMode };

// Largest fraction of a revolution the Fill processor treats as a real
// sweep gap to interpolate across. A larger jump is a duplicate angle, a
// backward step, or a discontinuity (mode switch, heading-source change),
// not a gap — filling it would repaint most of the PPI with one spoke.
// Mirrors NORTH_UP_MAX_FILL_FRACTION in ppi.js.
const MAX_GAP_FRACTION = 1 / 32;

/**
 * Spoke processing modes - use these constants instead of string literals.
 * Order matches server-side enum: 0=Clean, 1=Fill, 2=Reduce, 3=Smooth
 */
const SpokeProcessingMode = Object.freeze({
  CLEAN: "clean",
  FILL: "fill",
  REDUCE: "reduce",
  SMOOTH: "smooth",

  // Array for index-based lookup from server values
  fromIndex(index) {
    const modes = [this.CLEAN, this.FILL, this.REDUCE, this.SMOOTH];
    return modes[index] ?? this.CLEAN;
  },
});

/**
 * Base class for spoke processing strategies
 */
class SpokeProcessor {
  constructor(spokesPerRevolution, legend) {
    this.spokesPerRevolution = spokesPerRevolution;
    this.legend = legend;
    this.rotationCount = 0;
    this.lastSpokeAngle = -1;
    this.firstSpokeAngle = -1;
  }

  /**
   * Reset state when clearing display or changing range
   */
  reset() {
    this.rotationCount = 0;
    this.lastSpokeAngle = -1;
    this.firstSpokeAngle = -1;
  }

  /**
   * Update rotation tracking
   * @param {number} spokeAngle - Current spoke angle
   * @param {number} spokesPerRevolution - Total spokes per revolution
   * @returns {boolean} - True if rotation wrapped around
   */
  updateRotationTracking(spokeAngle, spokesPerRevolution) {
    if (this.firstSpokeAngle < 0) {
      this.firstSpokeAngle = spokeAngle;
    }

    // Detect when we wrap around from high angle to low angle
    const wrapped =
      this.lastSpokeAngle >= 0 &&
      spokeAngle < this.lastSpokeAngle - spokesPerRevolution / 2;

    if (wrapped) {
      this.rotationCount++;
    }

    this.lastSpokeAngle = spokeAngle;
    return wrapped;
  }

  /**
   * Check if processor needs to wait for a full rotation before displaying
   * after standby/range change (to flush stale buffered spokes)
   * @returns {boolean}
   */
  needsRotationWait() {
    return false; // Override in subclasses if needed
  }

  /**
   * Drop any state tracked in the write-angle (display) space. Called by
   * the PPI when the write frame changes without a full reset — e.g. a
   * head-up/north-up switch remaps the raster rows, so display-space
   * tracking from the old frame (like Fill's gap anchor) is meaningless.
   */
  resetWriteTracking() {}

  /**
   * Process spoke data into the buffer
   * @param {Uint8Array} buffer - Output buffer
   * @param {Object} spoke - Spoke data with angle and data
   * @param {number} spokesPerRevolution - Total spokes per revolution
   * @param {number} maxSpokeLength - Maximum spoke length
   * @param {number} [writeAngle] - Display row to write instead of
   *   spoke.angle (same units as spoke.angle). Used by north-up mode to
   *   place the spoke at its geographic bearing. All buffer indexing
   *   (row offset, neighbor rows, gap fill) must use this; per-sweep
   *   bookkeeping (reduce calibration, rotation tracking) must stay on
   *   the relative spoke.angle, which is monotonic while writeAngle is
   *   not when the boat yaws.
   */
  processSpoke(buffer, spoke, spokesPerRevolution, maxSpokeLength, writeAngle) {
    throw new Error("processSpoke must be implemented by subclass");
  }
}

/**
 * Clean processor - no smoothing, displays spokes as-is immediately
 */
class CleanSpokeProcessor extends SpokeProcessor {
  processSpoke(buffer, spoke, spokesPerRevolution, maxSpokeLength, writeAngle) {
    const angle = writeAngle ?? spoke.angle;
    const offset = angle * maxSpokeLength;
    const spokeLen = spoke.data.length;

    // Bounds check
    if (offset + spokeLen > buffer.length) {
      console.error(
        `Buffer overflow: offset=${offset}, data.len=${spokeLen}, buf.len=${buffer.length}`
      );
      return;
    }

    // Write spoke data directly without modification
    for (let i = 0; i < spokeLen; i++) {
      buffer[offset + i] = spoke.data[i];
    }

    // Clear remainder of spoke if data is shorter than max
    if (spokeLen < maxSpokeLength) {
      buffer.fill(0, offset + spokeLen, offset + maxSpokeLength);
    }
  }
}

/**
 * Fill processor - fills gaps between received spokes by repeating data
 * Used when radar reports more spokes than it actually sends
 */
class FillSpokeProcessor extends SpokeProcessor {
  constructor(spokesPerRevolution, legend) {
    super(spokesPerRevolution, legend);
    this.lastProcessedAngle = -1;
  }

  reset() {
    super.reset();
    this.lastProcessedAngle = -1;
  }

  resetWriteTracking() {
    this.lastProcessedAngle = -1;
  }

  processSpoke(buffer, spoke, spokesPerRevolution, maxSpokeLength, writeAngle) {
    const angle = writeAngle ?? spoke.angle;
    const spokeLen = spoke.data.length;

    // Fill gaps by repeating spoke data for missing angles. Tracked in
    // the same (display) space the rows are written in, or the gap math
    // would fill the wrong rows in north-up mode.
    if (this.lastProcessedAngle >= 0) {
      const gap = this.#calculateGap(
        this.lastProcessedAngle,
        angle,
        spokesPerRevolution
      );
      if (gap > 1) {
        this.#fillGap(
          buffer,
          spoke,
          this.lastProcessedAngle,
          gap,
          spokesPerRevolution,
          maxSpokeLength
        );
      }
    }
    this.lastProcessedAngle = angle;

    // Write the actual spoke
    const offset = angle * maxSpokeLength;

    // Bounds check
    if (offset + spokeLen > buffer.length) {
      console.error(
        `Buffer overflow: offset=${offset}, data.len=${spokeLen}, buf.len=${buffer.length}`
      );
      return;
    }

    for (let i = 0; i < spokeLen; i++) {
      buffer[offset + i] = spoke.data[i];
    }

    if (spokeLen < maxSpokeLength) {
      buffer.fill(0, offset + spokeLen, offset + maxSpokeLength);
    }
  }

  #calculateGap(lastAngle, currentAngle, spokesPerRevolution) {
    // Forward distance around the circle. Anything beyond the clamp is
    // not a sweep gap: a duplicate angle (DRS4D sends each angle twice),
    // a backward step (north-up display angle moves against the sweep
    // while the boat yaws), or a discontinuity (mode switch, heading
    // source change). Filling those would repaint up to the whole PPI
    // with one spoke's data, so report "no gap" instead.
    const gap =
      (currentAngle - lastAngle + spokesPerRevolution) % spokesPerRevolution;
    if (gap >= spokesPerRevolution * MAX_GAP_FRACTION) {
      return 0;
    }
    return gap;
  }

  #fillGap(buffer, spoke, lastAngle, gap, spokesPerRevolution, maxSpokeLength) {
    const spokeLen = spoke.data.length;

    for (let i = 1; i < gap; i++) {
      const fillAngle = (lastAngle + i) % spokesPerRevolution;
      const offset = fillAngle * maxSpokeLength;

      if (offset + spokeLen > buffer.length) {
        continue;
      }

      for (let j = 0; j < spokeLen; j++) {
        buffer[offset + j] = spoke.data[j];
      }

      if (spokeLen < maxSpokeLength) {
        buffer.fill(0, offset + spokeLen, offset + maxSpokeLength);
      }
    }
  }
}

/**
 * Reduce processor - reduces spoke count by counting actual spokes and resizing buffer
 * Counts actual spokes during first 2 revolutions, then resizes buffer and scales angles
 *
 * Furuno radars report 8192 spokes but send fewer (e.g., 2048). The angles still range
 * from 0 to ~8191 (spread across the full range), so we need to:
 * 1. Count how many spokes are actually sent per revolution
 * 2. Resize buffer to that count
 * 3. Scale incoming angles to fit the reduced buffer
 */
class ReduceSpokeProcessor extends SpokeProcessor {
  constructor(spokesPerRevolution, legend) {
    super(spokesPerRevolution, legend);
    this.calibrated = false;
    this.calibrationRevolutions = 0;
    this.calibrationLastAngle = -1;
    this.revolutionCounts = [];
    this.currentRevolutionCount = 0;
    this.actualSpokesPerRevolution = spokesPerRevolution;
    this.reportedSpokesPerRevolution = spokesPerRevolution;
    this.onCalibrationComplete = null;
  }

  reset() {
    super.reset();
    // Don't reset calibration state - once we know the actual spoke count,
    // it doesn't change with range changes. Only reset tracking state.
    this.calibrationLastAngle = -1;
    this.currentRevolutionCount = 0;
  }

  /**
   * Full reset including calibration - use when switching processors
   */
  fullReset() {
    super.reset();
    this.calibrated = false;
    this.calibrationRevolutions = 0;
    this.calibrationLastAngle = -1;
    this.revolutionCounts = [];
    this.currentRevolutionCount = 0;
  }

  needsRotationWait() {
    return true;
  }

  /**
   * Set callback for when calibration completes and buffer resize is needed
   * @param {function(number): void} callback - Called with actualSpokesPerRevolution
   */
  setCalibrationCallback(callback) {
    this.onCalibrationComplete = callback;
  }

  processSpoke(buffer, spoke, spokesPerRevolution, maxSpokeLength, writeAngle) {
    // Track calibration during first 2 revolutions. Calibration must see
    // the relative spoke.angle, never the display writeAngle: heading
    // motion would fire false wrap detections and skew the spoke count,
    // and a miscalibration is sticky across range changes.
    if (!this.calibrated) {
      this.#updateCalibration(spoke.angle, spokesPerRevolution);
      // During calibration, don't draw (wait for buffer resize)
      return;
    }

    // After calibration, scale angle from reported range to actual buffer size
    // e.g., if reported=8192, actual=2048, angle 4096 -> 1024.
    // writeAngle is in the same reported units, so it scales identically.
    const scaledAngle = Math.floor(
      ((writeAngle ?? spoke.angle) * this.actualSpokesPerRevolution) /
        this.reportedSpokesPerRevolution
    );

    if (scaledAngle >= this.actualSpokesPerRevolution) {
      return;
    }

    const offset = scaledAngle * maxSpokeLength;
    const spokeLen = spoke.data.length;

    // Bounds check against buffer
    if (offset + spokeLen > buffer.length) {
      return;
    }

    for (let i = 0; i < spokeLen; i++) {
      buffer[offset + i] = spoke.data[i];
    }

    if (spokeLen < maxSpokeLength) {
      const endOffset = Math.min(offset + maxSpokeLength, buffer.length);
      buffer.fill(0, offset + spokeLen, endOffset);
    }
  }

  #updateCalibration(angle, spokesPerRevolution) {
    // Detect revolution completion (angle wraps from high to low)
    if (this.calibrationLastAngle >= 0) {
      const wrapThreshold = spokesPerRevolution / 2;
      if (this.calibrationLastAngle > wrapThreshold && angle < wrapThreshold) {
        // Revolution completed
        const revolutionSpokeCount = this.currentRevolutionCount;
        this.currentRevolutionCount = 0;

        this.revolutionCounts.push(revolutionSpokeCount);
        this.calibrationRevolutions++;

        console.log(
          `Reduce processor: calibration revolution ${this.calibrationRevolutions} ` +
            `had ${revolutionSpokeCount} unique angles (reported ${spokesPerRevolution})`
        );

        // After 2 revolutions, calibrate
        if (this.calibrationRevolutions === 2) {
          // Use the maximum spoke count seen across revolutions
          const actualSpokes = Math.max(...this.revolutionCounts);

          this.calibrated = true;
          this.actualSpokesPerRevolution = actualSpokes;
          this.reportedSpokesPerRevolution = spokesPerRevolution;

          console.log(
            `Reduce processor: calibrated to ${actualSpokes} spokes per revolution ` +
              `(reported was ${spokesPerRevolution}, ratio ${(spokesPerRevolution / actualSpokes).toFixed(2)})`
          );

          if (this.onCalibrationComplete) {
            this.onCalibrationComplete(actualSpokes);
          }
        }
      }
    }

    // The DRS4D-NXT sends each angle twice in consecutive sweeps (observed
    // angles step 7560, 7560, 7574, 7574, ...). Counting both copies would
    // double the calibrated spoke count and leave every other slot empty
    // in the reduced buffer, producing radial striping across the PPI.
    // Only count an angle as a new spoke when it differs from the last.
    if (angle !== this.calibrationLastAngle) {
      this.currentRevolutionCount++;
    }
    this.calibrationLastAngle = angle;
  }

  getActualSpokesPerRevolution() {
    return this.actualSpokesPerRevolution;
  }

  isCalibrated() {
    return this.calibrated;
  }
}

/**
 * Smoothing processor - uses neighbor enhancement and filtering
 * Waits for several rotations before applying aggressive filtering
 */
class SmoothingSpokeProcessor extends SpokeProcessor {
  constructor(spokesPerRevolution, legend) {
    super(spokesPerRevolution, legend);
    this.fillRotations = 4; // Number of rotations to use neighbor enhancement
  }

  needsRotationWait() {
    // Smoothing needs to wait for a full rotation to flush stale buffered spokes
    return true;
  }

  processSpoke(buffer, spoke, spokesPerRevolution, maxSpokeLength, writeAngle) {
    // Center row and all neighbor rows must come from the same (display)
    // angle, or the smoothing would deposit energy `heading` rows away
    // from the spoke in north-up mode.
    const angle = writeAngle ?? spoke.angle;
    const offset = angle * maxSpokeLength;
    const spokeLen = spoke.data.length;

    // Bounds check
    if (offset + spokeLen > buffer.length) {
      console.error(
        `Buffer overflow: offset=${offset}, data.len=${spokeLen}, buf.len=${buffer.length}`
      );
      return;
    }

    const strongReturn = this.legend.strongReturn;
    const mediumReturn = this.legend.mediumReturn;
    const lowReturn = this.legend.lowReturn;
    const specialStart = this.legend.specialStart;
    const maxNormal = specialStart - 1;

    // Only use neighbor enhancement during first few rotations to fill display quickly
    if (this.rotationCount < this.fillRotations) {
      this.#neighborEnhancement(
        buffer,
        spoke,
        angle,
        spokesPerRevolution,
        maxSpokeLength,
        offset,
        spokeLen,
        strongReturn,
        mediumReturn,
        specialStart
      );
    } else {
      this.#smartFiltering(
        buffer,
        spoke,
        angle,
        spokesPerRevolution,
        maxSpokeLength,
        offset,
        spokeLen,
        strongReturn,
        mediumReturn,
        lowReturn,
        specialStart,
        maxNormal
      );
    }

    // Clear remainder of spoke if data is shorter than max
    if (spokeLen < maxSpokeLength) {
      buffer.fill(0, offset + spokeLen, offset + maxSpokeLength);
    }
  }

  #neighborEnhancement(
    buffer,
    spoke,
    angle,
    spokesPerRevolution,
    maxSpokeLength,
    offset,
    spokeLen,
    strongReturn,
    mediumReturn,
    specialStart
  ) {
    for (let i = 0; i < spokeLen; i++) {
      const val = spoke.data[i];
      // Write current spoke at full value
      buffer[offset + i] = val;

      if (val >= specialStart) {
        // Leave things like history, doppler etc as they are
        continue;
      }

      if (val > 1) {
        // Strong signals (>60): spread wide (±6 spokes) with higher intensity
        // Medium signals (25-60): spread medium (±4 spokes)
        // Weak signals (<25): spread narrow (±2 spokes) with lower intensity
        let spreadWidth, blendFactors;

        if (val > strongReturn) {
          // Strong signal - spread wide and strong
          spreadWidth = 6;
          blendFactors = [0.95, 0.88, 0.78, 0.65, 0.5, 0.35];
        } else if (val > mediumReturn) {
          // Medium signal - normal spread
          spreadWidth = 4;
          blendFactors = [0.85, 0.65, 0.45, 0.25];
        } else {
          // Weak signal - narrow spread, lower intensity
          spreadWidth = 2;
          blendFactors = [0.6, 0.3];
        }

        // Spread to neighboring spokes (both directions)
        for (let d = 1; d <= spreadWidth; d++) {
          const prev = (angle + spokesPerRevolution - d) % spokesPerRevolution;
          const next = (angle + d) % spokesPerRevolution;
          const prevOffset = prev * maxSpokeLength;
          const nextOffset = next * maxSpokeLength;
          const blendVal = Math.floor(val * blendFactors[d - 1]);

          if (buffer[prevOffset + i] < blendVal) {
            buffer[prevOffset + i] = blendVal;
          }
          if (buffer[nextOffset + i] < blendVal) {
            buffer[nextOffset + i] = blendVal;
          }
        }
      }
    }
  }

  #smartFiltering(
    buffer,
    spoke,
    angle,
    spokesPerRevolution,
    maxSpokeLength,
    offset,
    spokeLen,
    strongReturn,
    mediumReturn,
    lowReturn,
    specialStart,
    maxNormal
  ) {
    // Smart filtering
    // - Strong signals with neighbor support get amplified aggressively (wide check ±4)
    // - Isolated weak signals (scatter) get killed

    // Wide neighbor check for strong signals: ±4 spokes
    const prev1Offset =
      ((angle + spokesPerRevolution - 1) % spokesPerRevolution) *
      maxSpokeLength;
    const prev2Offset =
      ((angle + spokesPerRevolution - 2) % spokesPerRevolution) *
      maxSpokeLength;
    const prev3Offset =
      ((angle + spokesPerRevolution - 3) % spokesPerRevolution) *
      maxSpokeLength;
    const prev4Offset =
      ((angle + spokesPerRevolution - 4) % spokesPerRevolution) *
      maxSpokeLength;
    const next1Offset = ((angle + 1) % spokesPerRevolution) * maxSpokeLength;
    const next2Offset = ((angle + 2) % spokesPerRevolution) * maxSpokeLength;
    const next3Offset = ((angle + 3) % spokesPerRevolution) * maxSpokeLength;
    const next4Offset = ((angle + 4) % spokesPerRevolution) * maxSpokeLength;

    for (let i = 0; i < spokeLen; i++) {
      const val = spoke.data[i];

      // Check neighbor support (from previous rotation's data still in buffer)
      const prev1 = buffer[prev1Offset + i];
      const prev2 = buffer[prev2Offset + i];
      const prev3 = buffer[prev3Offset + i];
      const prev4 = buffer[prev4Offset + i];
      const next1 = buffer[next1Offset + i];
      const next2 = buffer[next2Offset + i];
      const next3 = buffer[next3Offset + i];
      const next4 = buffer[next4Offset + i];

      // For strong signals: use wide sum (±4)
      const wideSum =
        prev1 + prev2 + prev3 + prev4 + next1 + next2 + next3 + next4;
      const wideMax = Math.max(
        prev1,
        prev2,
        prev3,
        prev4,
        next1,
        next2,
        next3,
        next4
      );
      // For weak signals: use narrow sum (±2)
      const narrowSum = prev1 + prev2 + next1 + next2;
      const narrowMax = Math.max(prev1, prev2, next1, next2);

      let outputVal;

      if (val > strongReturn) {
        // Strong signal: use wide neighbor check (±4)
        if (wideSum > 3 * strongReturn) {
          // Solid mass - boost hard and spread to neighbors
          outputVal = Math.min(maxNormal, Math.floor(val * 1.35));
          // Boost immediate neighbors to fill gaps
          if (prev1 > mediumReturn)
            buffer[prev1Offset + i] = Math.min(
              maxNormal,
              Math.floor(prev1 * 1.15)
            );
          if (next1 > mediumReturn)
            buffer[next1Offset + i] = Math.min(
              maxNormal,
              Math.floor(next1 * 1.15)
            );
          if (prev2 > mediumReturn)
            buffer[prev2Offset + i] = Math.min(
              maxNormal,
              Math.floor(prev2 * 1.1)
            );
          if (next2 > mediumReturn)
            buffer[next2Offset + i] = Math.min(
              maxNormal,
              Math.floor(next2 * 1.1)
            );
        } else if (wideMax > mediumReturn) {
          // Some support - moderate boost
          outputVal = Math.min(maxNormal, Math.floor(val * 1.2));
        } else {
          // Strong but isolated - suspicious, reduce
          outputVal = Math.floor(val * 0.8);
        }
      } else if (val > mediumReturn) {
        // Medium signal: needs good neighbor support
        if (narrowSum > 3 * mediumReturn) {
          // Good support - boost it
          outputVal = Math.min(maxNormal, Math.floor(val * 1.2));
        } else if (narrowMax > 2 * mediumReturn) {
          // Some support - keep
          outputVal = val;
        } else {
          // Isolated medium - likely scatter, punish hard
          outputVal = Math.floor(val * 0.4);
        }
      } else if (val > 1) {
        // Weak signal: kill it unless very well supported
        if (narrowSum > 3 * mediumReturn) {
          // Strong neighbors - this might be edge of real target
          outputVal = val;
        } else if (narrowMax > strongReturn) {
          // Next to something strong - keep faint
          outputVal = Math.floor(val * 0.5);
        } else {
          // Isolated weak signal - kill it
          outputVal = 0;
        }
      } else {
        outputVal = val;
      }

      buffer[offset + i] = outputVal;
    }
  }
}

/**
 * Factory for creating spoke processors
 */
class SpokeProcessorFactory {
  /**
   * Create a spoke processor
   * @param {string} mode - SpokeProcessingMode value or "auto"
   * @param {number} spokesPerRevolution - Number of spokes per revolution
   * @param {Object} legend - Legend with strongReturn, mediumReturn, etc.
   * @returns {SpokeProcessor}
   */
  static create(mode, spokesPerRevolution, legend) {
    if (mode === "auto") {
      // Auto-detect: <= 2048 spokes use clean, > 2048 use smooth
      mode =
        spokesPerRevolution <= 2048
          ? SpokeProcessingMode.CLEAN
          : SpokeProcessingMode.SMOOTH;
    }

    switch (mode) {
      case SpokeProcessingMode.CLEAN:
        return new CleanSpokeProcessor(spokesPerRevolution, legend);
      case SpokeProcessingMode.FILL:
        return new FillSpokeProcessor(spokesPerRevolution, legend);
      case SpokeProcessingMode.REDUCE:
        return new ReduceSpokeProcessor(spokesPerRevolution, legend);
      case SpokeProcessingMode.SMOOTH:
        return new SmoothingSpokeProcessor(spokesPerRevolution, legend);
      default:
        console.warn(`Unknown spoke processor mode: ${mode}, using clean`);
        return new CleanSpokeProcessor(spokesPerRevolution, legend);
    }
  }

  /**
   * Get list of available modes in order
   * @returns {string[]}
   */
  static getModes() {
    return [
      SpokeProcessingMode.CLEAN,
      SpokeProcessingMode.FILL,
      SpokeProcessingMode.REDUCE,
      SpokeProcessingMode.SMOOTH,
    ];
  }
}
