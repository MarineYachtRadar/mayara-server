/**
 * Blob Detector using Connected Component Labeling (Two-Pass Algorithm)
 *
 * Finds connected regions in radar spoke data that are above a threshold.
 * Uses Union-Find for efficient merging of connected components.
 */

export class BlobDetector {
  /**
   * @param {number} maxSpokeLen - Width of the polar image (radial dimension)
   * @param {number} spokesPerRev - Height of the polar image (angular dimension)
   * @param {number} threshold - Minimum value to consider as "on" (0-255)
   * @param {number} minBlobSize - Minimum number of pixels for a valid blob
   */
  constructor(maxSpokeLen, spokesPerRev, threshold = 10, minBlobSize = 20) {
    this.width = maxSpokeLen;
    this.height = spokesPerRev;
    this.threshold = threshold;
    this.minBlobSize = minBlobSize;

    // Reusable buffers to avoid GC pressure
    this.labels = new Int32Array(this.width * this.height);
    this.parent = new Int32Array(this.width * this.height);
    this.rank = new Uint8Array(this.width * this.height);
  }

  /**
   * Union-Find: Find root with path compression
   */
  find(x) {
    if (this.parent[x] !== x) {
      this.parent[x] = this.find(this.parent[x]);
    }
    return this.parent[x];
  }

  /**
   * Union-Find: Union by rank
   */
  union(x, y) {
    const rootX = this.find(x);
    const rootY = this.find(y);

    if (rootX === rootY) return;

    if (this.rank[rootX] < this.rank[rootY]) {
      this.parent[rootX] = rootY;
    } else if (this.rank[rootX] > this.rank[rootY]) {
      this.parent[rootY] = rootX;
    } else {
      this.parent[rootY] = rootX;
      this.rank[rootX]++;
    }
  }

  /**
   * Detect blobs in spoke data
   *
   * @param {Uint8Array} spokeData - Raw spoke data (width * height bytes)
   * @returns {Array<Blob>} Array of detected blobs
   */
  detect(spokeData) {
    const { width, height, threshold, labels, parent, rank } = this;

    // Reset buffers
    labels.fill(0);

    let nextLabel = 1;

    // === Pass 1: Initial labeling with Union-Find ===
    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        const idx = y * width + x;

        // Skip pixels below threshold
        if (spokeData[idx] < threshold) {
          continue;
        }

        // Get neighbor labels (4-connectivity: left and up)
        // For polar data, we also handle angular wraparound
        const leftIdx = x > 0 ? idx - 1 : -1;
        const upIdx = y > 0 ? idx - width : (height - 1) * width + x; // Wraparound!

        const leftLabel = leftIdx >= 0 && spokeData[leftIdx] >= threshold ? labels[leftIdx] : 0;
        const upLabel = spokeData[upIdx] >= threshold ? labels[upIdx] : 0;

        if (leftLabel === 0 && upLabel === 0) {
          // New component
          labels[idx] = nextLabel;
          parent[nextLabel] = nextLabel;
          rank[nextLabel] = 0;
          nextLabel++;
        } else if (leftLabel !== 0 && upLabel === 0) {
          labels[idx] = leftLabel;
        } else if (leftLabel === 0 && upLabel !== 0) {
          labels[idx] = upLabel;
        } else {
          // Both neighbors have labels - use the smaller and union them
          labels[idx] = Math.min(leftLabel, upLabel);
          if (leftLabel !== upLabel) {
            this.union(leftLabel, upLabel);
          }
        }
      }
    }

    // === Pass 2: Flatten labels and collect blob data ===
    const blobPixels = new Map(); // rootLabel -> array of {x, y, value}
    const blobBounds = new Map(); // rootLabel -> {minX, maxX, minY, maxY}

    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        const idx = y * width + x;
        const label = labels[idx];

        if (label === 0) continue;

        // Get root label
        const root = this.find(label);
        labels[idx] = root; // Flatten for future use

        // Collect pixel
        if (!blobPixels.has(root)) {
          blobPixels.set(root, []);
          blobBounds.set(root, {
            minX: x, maxX: x,
            minY: y, maxY: y,
            sumValue: 0
          });
        }

        const pixels = blobPixels.get(root);
        const bounds = blobBounds.get(root);
        const value = spokeData[idx];

        pixels.push({ x, y, value });

        bounds.minX = Math.min(bounds.minX, x);
        bounds.maxX = Math.max(bounds.maxX, x);
        bounds.minY = Math.min(bounds.minY, y);
        bounds.maxY = Math.max(bounds.maxY, y);
        bounds.sumValue += value;
      }
    }

    // === Build blob objects, filtering by minimum size ===
    const blobs = [];

    for (const [label, pixels] of blobPixels) {
      if (pixels.length < this.minBlobSize) continue;

      const bounds = blobBounds.get(label);

      blobs.push({
        label,
        pixels,
        pixelCount: pixels.length,
        bounds: {
          minR: bounds.minX / width,      // Normalized radial
          maxR: bounds.maxX / width,
          minTheta: bounds.minY / height, // Normalized angular
          maxTheta: bounds.maxY / height,
        },
        // Bounding box in pixel coordinates
        bbox: {
          x: bounds.minX,
          y: bounds.minY,
          width: bounds.maxX - bounds.minX + 1,
          height: bounds.maxY - bounds.minY + 1,
        },
        // Average intensity
        avgIntensity: bounds.sumValue / pixels.length,
        // Center of mass (in polar coords, normalized)
        centroid: this.#computeCentroid(pixels, width, height),
      });
    }

    // Sort by size (largest first)
    blobs.sort((a, b) => b.pixelCount - a.pixelCount);

    // Merge nearby blobs (within mergeDistance pixels)
    const mergeDistance = 20; // pixels
    const mergedBlobs = this.#mergeNearbyBlobs(blobs, mergeDistance, width, height);

    return mergedBlobs;
  }

  /**
   * Merge blobs that are within a certain distance of each other
   * Uses bounding box proximity check for efficiency
   */
  #mergeNearbyBlobs(blobs, distance, width, height) {
    if (blobs.length <= 1) return blobs;

    // Track which blobs have been merged
    const merged = new Array(blobs.length).fill(false);
    const result = [];

    for (let i = 0; i < blobs.length; i++) {
      if (merged[i]) continue;

      // Start a new merged blob with blob i
      let mergedPixels = [...blobs[i].pixels];
      let mergedBounds = { ...blobs[i].bbox };
      let sumValue = blobs[i].avgIntensity * blobs[i].pixelCount;
      merged[i] = true;

      // Find all blobs that should be merged with this one
      let foundMerge = true;
      while (foundMerge) {
        foundMerge = false;

        for (let j = i + 1; j < blobs.length; j++) {
          if (merged[j]) continue;

          // Check if blob j is close enough to the current merged blob
          if (this.#blobsAreClose(mergedBounds, blobs[j].bbox, distance, height)) {
            // Merge blob j
            mergedPixels = mergedPixels.concat(blobs[j].pixels);
            sumValue += blobs[j].avgIntensity * blobs[j].pixelCount;

            // Update merged bounds
            mergedBounds.x = Math.min(mergedBounds.x, blobs[j].bbox.x);
            mergedBounds.y = Math.min(mergedBounds.y, blobs[j].bbox.y);
            const maxX1 = mergedBounds.x + mergedBounds.width;
            const maxX2 = blobs[j].bbox.x + blobs[j].bbox.width;
            const maxY1 = mergedBounds.y + mergedBounds.height;
            const maxY2 = blobs[j].bbox.y + blobs[j].bbox.height;
            mergedBounds.width = Math.max(maxX1, maxX2) - mergedBounds.x;
            mergedBounds.height = Math.max(maxY1, maxY2) - mergedBounds.y;

            merged[j] = true;
            foundMerge = true;
          }
        }
      }

      // Create merged blob
      result.push({
        label: blobs[i].label,
        pixels: mergedPixels,
        pixelCount: mergedPixels.length,
        bounds: {
          minR: mergedBounds.x / width,
          maxR: (mergedBounds.x + mergedBounds.width) / width,
          minTheta: mergedBounds.y / height,
          maxTheta: (mergedBounds.y + mergedBounds.height) / height,
        },
        bbox: mergedBounds,
        avgIntensity: sumValue / mergedPixels.length,
        centroid: this.#computeCentroid(mergedPixels, width, height),
      });
    }

    // Sort by size again
    result.sort((a, b) => b.pixelCount - a.pixelCount);

    return result;
  }

  /**
   * Check if two blob bounding boxes are within distance of each other
   * Handles angular wraparound
   */
  #blobsAreClose(bbox1, bbox2, distance, height) {
    // Radial distance check (x dimension)
    const x1Min = bbox1.x;
    const x1Max = bbox1.x + bbox1.width;
    const x2Min = bbox2.x;
    const x2Max = bbox2.x + bbox2.width;

    const radialGap = Math.max(0, Math.max(x1Min - x2Max, x2Min - x1Max));
    if (radialGap > distance) return false;

    // Angular distance check (y dimension) - with wraparound
    const y1Min = bbox1.y;
    const y1Max = bbox1.y + bbox1.height;
    const y2Min = bbox2.y;
    const y2Max = bbox2.y + bbox2.height;

    // Direct gap
    const angularGap1 = Math.max(0, Math.max(y1Min - y2Max, y2Min - y1Max));

    // Wraparound gap (blob1 near end, blob2 near start or vice versa)
    const angularGap2 = Math.max(0, y1Min + (height - y2Max));
    const angularGap3 = Math.max(0, y2Min + (height - y1Max));

    const angularGap = Math.min(angularGap1, angularGap2, angularGap3);

    return angularGap <= distance;
  }

  /**
   * Compute centroid of blob in normalized polar coordinates
   */
  #computeCentroid(pixels, width, height) {
    let sumX = 0, sumY = 0, sumWeight = 0;

    for (const p of pixels) {
      const weight = p.value;
      sumX += p.x * weight;
      sumY += p.y * weight;
      sumWeight += weight;
    }

    if (sumWeight === 0) {
      return { r: 0.5, theta: 0.5 };
    }

    return {
      r: (sumX / sumWeight) / width,
      theta: (sumY / sumWeight) / height,
    };
  }

  /**
   * Update detection threshold
   */
  setThreshold(threshold) {
    this.threshold = threshold;
  }

  /**
   * Update minimum blob size
   */
  setMinBlobSize(minSize) {
    this.minBlobSize = minSize;
  }
}

/**
 * Blob object structure:
 * {
 *   label: number,           // Unique ID for this blob
 *   pixels: Array<{x, y, value}>,  // All pixels in the blob
 *   pixelCount: number,      // Number of pixels
 *   bounds: {                // Normalized polar coordinates [0, 1]
 *     minR, maxR,            // Radial bounds
 *     minTheta, maxTheta,    // Angular bounds
 *   },
 *   bbox: {                  // Pixel coordinates
 *     x, y, width, height,
 *   },
 *   avgIntensity: number,    // Average pixel value
 *   centroid: {r, theta},    // Center of mass (normalized)
 * }
 */
