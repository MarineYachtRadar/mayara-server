/**
 * Contour Tracer using Moore Neighborhood Algorithm
 *
 * Extracts polygon contours from blobs detected by BlobDetector.
 * Returns vertices in both polar and Cartesian coordinates.
 */

export class ContourTracer {
  /**
   * @param {number} maxSpokeLen - Width of the polar image (radial dimension)
   * @param {number} spokesPerRev - Height of the polar image (angular dimension)
   */
  constructor(maxSpokeLen, spokesPerRev) {
    this.width = maxSpokeLen;
    this.height = spokesPerRev;

    // Moore neighborhood: 8 directions starting from right, going clockwise
    // [dx, dy] for each direction
    this.directions = [
      [1, 0],   // 0: right
      [1, 1],   // 1: down-right
      [0, 1],   // 2: down
      [-1, 1],  // 3: down-left
      [-1, 0],  // 4: left
      [-1, -1], // 5: up-left
      [0, -1],  // 6: up
      [1, -1],  // 7: up-right
    ];
  }

  /**
   * Trace contour of a blob
   *
   * @param {Object} blob - Blob from BlobDetector
   * @param {Uint8Array} spokeData - Original spoke data
   * @param {number} threshold - Threshold used for detection
   * @returns {Object} Contour with polar and Cartesian vertices
   */
  trace(blob, spokeData, threshold) {
    const { width, height, directions } = this;

    // Create a set of blob pixels for fast lookup
    const blobPixelSet = new Set();
    for (const p of blob.pixels) {
      blobPixelSet.add(`${p.x},${p.y}`);
    }

    // Find starting point: leftmost pixel in the blob (smallest x)
    let startX = Infinity, startY = 0;
    for (const p of blob.pixels) {
      if (p.x < startX) {
        startX = p.x;
        startY = p.y;
      }
    }

    // If no valid start found, return empty contour
    if (startX === Infinity) {
      return { polarVertices: [], cartesianVertices: [] };
    }

    // Moore neighborhood contour tracing
    const contourPixels = [];
    let currentX = startX;
    let currentY = startY;
    let backtrackDir = 4; // Start by coming from the left (so we first check right)

    const maxIterations = blob.pixels.length * 4; // Safety limit
    let iterations = 0;

    do {
      contourPixels.push({ x: currentX, y: currentY });

      // Start searching from the pixel we came from (backtrack direction)
      // Go clockwise around the current pixel
      let found = false;
      let startDir = (backtrackDir + 1) % 8;

      for (let i = 0; i < 8; i++) {
        const dir = (startDir + i) % 8;
        const [dx, dy] = directions[dir];
        let nx = currentX + dx;
        let ny = currentY + dy;

        // Handle angular wraparound
        if (ny < 0) ny = height - 1;
        if (ny >= height) ny = 0;

        // Check bounds for radial
        if (nx < 0 || nx >= width) continue;

        // Check if this neighbor is part of the blob
        if (blobPixelSet.has(`${nx},${ny}`)) {
          // Move to this pixel
          currentX = nx;
          currentY = ny;
          // Backtrack direction is opposite of the direction we moved
          backtrackDir = (dir + 4) % 8;
          found = true;
          break;
        }
      }

      if (!found) {
        // Isolated pixel or stuck
        break;
      }

      iterations++;
    } while (
      (currentX !== startX || currentY !== startY) &&
      iterations < maxIterations
    );

    // Convert to polar and Cartesian coordinates
    const polarVertices = [];
    const cartesianVertices = [];

    for (const p of contourPixels) {
      // Normalized polar coordinates
      const r = p.x / width;
      // Negative theta for clockwise rotation (marine radar convention)
      // Spoke 0 = North (top), increasing spoke = clockwise
      const theta = -(p.y / height) * 2 * Math.PI;

      polarVertices.push({ r, theta: p.y / height }); // theta normalized [0, 1]

      // Cartesian coordinates (centered at origin, range [-1, 1])
      const x = r * Math.cos(theta);
      const y = r * Math.sin(theta);
      cartesianVertices.push({ x, y });
    }

    return {
      polarVertices,
      cartesianVertices,
      pixelCount: contourPixels.length,
    };
  }

  /**
   * Trace contours for multiple blobs
   *
   * @param {Array<Object>} blobs - Blobs from BlobDetector
   * @param {Uint8Array} spokeData - Original spoke data
   * @param {number} threshold - Threshold used for detection
   * @returns {Array<Object>} Array of contours
   */
  traceAll(blobs, spokeData, threshold) {
    const contours = [];

    for (const blob of blobs) {
      const contour = this.trace(blob, spokeData, threshold);

      if (contour.cartesianVertices.length >= 3) {
        contours.push({
          blob,
          ...contour,
        });
      }
    }

    return contours;
  }
}

/**
 * Polygon Simplification using Douglas-Peucker Algorithm
 *
 * Reduces the number of vertices while preserving shape.
 */
export class PolygonSimplifier {
  /**
   * Simplify a polygon using Douglas-Peucker algorithm
   *
   * @param {Array<{x, y}>} vertices - Input vertices
   * @param {number} tolerance - Distance tolerance for simplification
   * @returns {Array<{x, y}>} Simplified vertices
   */
  static simplify(vertices, tolerance = 0.01) {
    if (vertices.length <= 2) return vertices;

    // Find the point with maximum distance from the line between first and last
    let maxDist = 0;
    let maxIndex = 0;

    const start = vertices[0];
    const end = vertices[vertices.length - 1];

    for (let i = 1; i < vertices.length - 1; i++) {
      const dist = this.perpendicularDistance(vertices[i], start, end);
      if (dist > maxDist) {
        maxDist = dist;
        maxIndex = i;
      }
    }

    // If max distance is greater than tolerance, recursively simplify
    if (maxDist > tolerance) {
      const left = this.simplify(vertices.slice(0, maxIndex + 1), tolerance);
      const right = this.simplify(vertices.slice(maxIndex), tolerance);

      // Combine results (remove duplicate point at maxIndex)
      return left.slice(0, -1).concat(right);
    } else {
      // All points between start and end are within tolerance
      return [start, end];
    }
  }

  /**
   * Calculate perpendicular distance from point to line
   */
  static perpendicularDistance(point, lineStart, lineEnd) {
    const dx = lineEnd.x - lineStart.x;
    const dy = lineEnd.y - lineStart.y;

    // Line length squared
    const lenSq = dx * dx + dy * dy;

    if (lenSq === 0) {
      // Start and end are the same point
      return Math.sqrt(
        (point.x - lineStart.x) ** 2 + (point.y - lineStart.y) ** 2
      );
    }

    // Calculate perpendicular distance using cross product
    const cross = Math.abs(
      (point.y - lineStart.y) * dx - (point.x - lineStart.x) * dy
    );

    return cross / Math.sqrt(lenSq);
  }

  /**
   * Simplify for closed polygons (handles wraparound)
   *
   * For closed polygons, we can't just use start-to-end line because
   * they're the same point. Instead, we find the point furthest from
   * the centroid and use that as a split point, then simplify both halves.
   */
  static simplifyPolygon(vertices, tolerance = 0.01) {
    if (vertices.length <= 4) return vertices;

    // For closed polygons, find the point furthest from centroid to split
    let cx = 0, cy = 0;
    for (const v of vertices) {
      cx += v.x;
      cy += v.y;
    }
    cx /= vertices.length;
    cy /= vertices.length;

    // Find the point furthest from centroid
    let maxDist = 0;
    let splitIndex = 0;
    for (let i = 0; i < vertices.length; i++) {
      const dx = vertices[i].x - cx;
      const dy = vertices[i].y - cy;
      const dist = dx * dx + dy * dy;
      if (dist > maxDist) {
        maxDist = dist;
        splitIndex = i;
      }
    }

    // Also find the point roughly opposite (furthest from split point)
    let maxDistFromSplit = 0;
    let oppositeIndex = 0;
    const splitPoint = vertices[splitIndex];
    for (let i = 0; i < vertices.length; i++) {
      const dx = vertices[i].x - splitPoint.x;
      const dy = vertices[i].y - splitPoint.y;
      const dist = dx * dx + dy * dy;
      if (dist > maxDistFromSplit) {
        maxDistFromSplit = dist;
        oppositeIndex = i;
      }
    }

    // Ensure splitIndex < oppositeIndex
    if (splitIndex > oppositeIndex) {
      [splitIndex, oppositeIndex] = [oppositeIndex, splitIndex];
    }

    // Split the polygon into two open paths and simplify each
    const path1 = vertices.slice(splitIndex, oppositeIndex + 1);
    const path2 = [...vertices.slice(oppositeIndex), ...vertices.slice(0, splitIndex + 1)];

    const simplified1 = this.simplify(path1, tolerance);
    const simplified2 = this.simplify(path2, tolerance);

    // Combine (remove duplicate endpoints)
    const result = simplified1.slice(0, -1).concat(simplified2.slice(0, -1));

    return result.length >= 3 ? result : vertices.slice(0, 4); // Fallback to first 4 points
  }
}

/**
 * Contour object structure:
 * {
 *   blob: Object,                    // Original blob reference
 *   polarVertices: Array<{r, theta}>, // Normalized polar coords
 *   cartesianVertices: Array<{x, y}>, // Cartesian coords [-1, 1]
 *   pixelCount: number,              // Number of contour pixels
 * }
 */

/**
 * Sector Extractor for Radar Blobs
 *
 * Creates filled sector/pie shapes from radar blobs.
 * Instead of tracing boundaries, it creates sectors from origin (0,0)
 * to the min/max angles and radii of each blob.
 * This produces filled wedge shapes like on real radar displays.
 */
export class SectorExtractor {
  /**
   * @param {number} maxSpokeLen - Width of the polar image (radial dimension)
   * @param {number} spokesPerRev - Height of the polar image (angular dimension)
   */
  constructor(maxSpokeLen, spokesPerRev) {
    this.width = maxSpokeLen;
    this.height = spokesPerRev;
  }

  /**
   * Extract a filled sector polygon from a blob
   *
   * @param {Object} blob - Blob from BlobDetector
   * @returns {Object} Sector with Cartesian vertices
   */
  extractSector(blob) {
    const { width, height } = this;
    const pixels = blob.pixels;

    if (pixels.length < 3) {
      return { polarVertices: [], cartesianVertices: [] };
    }

    // Find the angular and radial extents of the blob
    let minSpoke = Infinity, maxSpoke = -Infinity;
    let minRange = Infinity, maxRange = -Infinity;

    for (const p of pixels) {
      minSpoke = Math.min(minSpoke, p.y);
      maxSpoke = Math.max(maxSpoke, p.y);
      minRange = Math.min(minRange, p.x);
      maxRange = Math.max(maxRange, p.x);
    }

    // Handle angular wraparound (blob crosses spoke 0)
    // Check if blob spans more than half the circle - if so, it's probably wrapping
    const spokeSpan = maxSpoke - minSpoke;
    let wrapsAround = false;
    if (spokeSpan > height * 0.5) {
      // Blob wraps around - recalculate
      wrapsAround = true;
      minSpoke = Infinity;
      maxSpoke = -Infinity;
      for (const p of pixels) {
        // Shift angles so wraparound is in the middle
        const shifted = p.y < height / 2 ? p.y + height : p.y;
        minSpoke = Math.min(minSpoke, shifted);
        maxSpoke = Math.max(maxSpoke, shifted);
      }
    }

    // Normalized radii - with expansion factor to make sectors more visible
    // Expand radially by ~3x the blob's radial extent
    const radialExtent = (maxRange - minRange) / width;
    const radialExpand = Math.max(0.02, radialExtent * 1.5); // At least 2% of radius expansion
    const minR = Math.max(0, (minRange / width) - radialExpand);
    const maxR = Math.min(1, (maxRange / width) + radialExpand);

    // Angular span in spokes - expand by ~3x to make sectors wider
    const angularExtent = maxSpoke - minSpoke;
    const angularExpand = Math.max(10, angularExtent * 1.5); // At least 10 spokes expansion
    const expandedMinSpoke = minSpoke - angularExpand;
    const expandedMaxSpoke = maxSpoke + angularExpand;
    const spokeSpanActual = expandedMaxSpoke - expandedMinSpoke + 1;

    // Convert to radians - negative for clockwise rotation (marine radar convention)
    // Spoke 0 = North (top of screen, positive Y), increasing spoke = clockwise
    const minTheta = -(expandedMinSpoke / height) * 2 * Math.PI;
    const maxTheta = -(expandedMaxSpoke / height) * 2 * Math.PI;

    // Angular span in radians (always positive)
    const angularSpanRad = (spokeSpanActual / height) * 2 * Math.PI;

    // Create sector polygon: arc from minTheta to maxTheta at maxR,
    // then back at minR (or to origin if minR is small)
    const vertices = [];

    // Number of points along each arc - at least 4 for small arcs, more for larger spans
    // Ensure we have enough points to draw a visible arc
    const numArcPoints = Math.max(4, Math.ceil(angularSpanRad / (Math.PI / 32))); // ~32 points per half circle

    // If inner radius is very small, make it a pie slice from origin
    const useOrigin = minR < 0.08;
    const innerR = useOrigin ? 0 : minR;

    // Outer arc (from expandedMinSpoke to expandedMaxSpoke at maxR)
    // Use direct interpolation to ensure correct span
    for (let i = 0; i <= numArcPoints; i++) {
      const t = i / numArcPoints;
      // Interpolate the spoke angle, then convert to radians
      const spokeAngle = expandedMinSpoke + t * spokeSpanActual;
      const theta = -(spokeAngle / height) * 2 * Math.PI;
      const x = maxR * Math.cos(theta);
      const y = maxR * Math.sin(theta);
      vertices.push({ x, y });
    }

    // Inner arc (from expandedMaxSpoke back to expandedMinSpoke at innerR) or single point at origin
    if (useOrigin) {
      // Just add origin point
      vertices.push({ x: 0, y: 0 });
    } else {
      // Inner arc in reverse
      for (let i = numArcPoints; i >= 0; i--) {
        const t = i / numArcPoints;
        const spokeAngle = expandedMinSpoke + t * spokeSpanActual;
        const theta = -(spokeAngle / height) * 2 * Math.PI;
        const x = innerR * Math.cos(theta);
        const y = innerR * Math.sin(theta);
        vertices.push({ x, y });
      }
    }

    return {
      polarVertices: [], // Not needed for rendering
      cartesianVertices: vertices,
      pixelCount: vertices.length,
      bounds: { minR, maxR, minTheta, maxTheta, angularSpanRad, spokeSpan: spokeSpanActual, wrapsAround }
    };
  }

  /**
   * Extract sectors for multiple blobs
   *
   * @param {Array<Object>} blobs - Blobs from BlobDetector
   * @returns {Array<Object>} Array of sectors
   */
  extractAll(blobs) {
    const sectors = [];

    for (const blob of blobs) {
      const sector = this.extractSector(blob);

      if (sector.cartesianVertices.length >= 3) {
        sectors.push({
          blob,
          ...sector,
        });
      }
    }

    return sectors;
  }
}

/**
 * Convex Hull Extractor using Graham Scan Algorithm
 *
 * Creates a convex hull polygon from blob pixels.
 * Much faster than contour tracing and produces filled-looking shapes.
 */
export class ConvexHullExtractor {
  /**
   * @param {number} maxSpokeLen - Width of the polar image (radial dimension)
   * @param {number} spokesPerRev - Height of the polar image (angular dimension)
   */
  constructor(maxSpokeLen, spokesPerRev) {
    this.width = maxSpokeLen;
    this.height = spokesPerRev;
  }

  /**
   * Extract convex hull from a blob
   *
   * @param {Object} blob - Blob from BlobDetector
   * @returns {Object} Hull with polar and Cartesian vertices
   */
  extractHull(blob) {
    const { width, height } = this;
    const pixels = blob.pixels;

    if (pixels.length < 3) {
      return { polarVertices: [], cartesianVertices: [] };
    }

    // Convert pixels to Cartesian coordinates for hull computation
    const points = [];
    for (const p of pixels) {
      // Normalized polar coordinates
      const r = p.x / width;
      // Negative theta for clockwise rotation (marine radar convention)
      const theta = -(p.y / height) * 2 * Math.PI;

      // Cartesian coordinates (centered at origin, range [-1, 1])
      const x = r * Math.cos(theta);
      const y = r * Math.sin(theta);
      points.push({ x, y, r, thetaNorm: p.y / height });
    }

    // Compute convex hull using Graham scan
    const hull = this.grahamScan(points);

    if (hull.length < 3) {
      return { polarVertices: [], cartesianVertices: [] };
    }

    // Extract polar and Cartesian vertices from hull
    const polarVertices = hull.map(p => ({ r: p.r, theta: p.thetaNorm }));
    const cartesianVertices = hull.map(p => ({ x: p.x, y: p.y }));

    return {
      polarVertices,
      cartesianVertices,
      pixelCount: hull.length,
    };
  }

  /**
   * Graham Scan algorithm for convex hull
   *
   * @param {Array<{x, y, r, thetaNorm}>} points - Input points
   * @returns {Array} Convex hull points in CCW order
   */
  grahamScan(points) {
    if (points.length < 3) return points;

    // Find the point with lowest y (and leftmost if tie)
    let pivot = points[0];
    for (let i = 1; i < points.length; i++) {
      if (points[i].y < pivot.y || (points[i].y === pivot.y && points[i].x < pivot.x)) {
        pivot = points[i];
      }
    }

    // Sort points by polar angle with respect to pivot
    const sorted = points.slice().sort((a, b) => {
      if (a === pivot) return -1;
      if (b === pivot) return 1;

      const angleA = Math.atan2(a.y - pivot.y, a.x - pivot.x);
      const angleB = Math.atan2(b.y - pivot.y, b.x - pivot.x);

      if (angleA !== angleB) return angleA - angleB;

      // If same angle, closer point comes first
      const distA = (a.x - pivot.x) ** 2 + (a.y - pivot.y) ** 2;
      const distB = (b.x - pivot.x) ** 2 + (b.y - pivot.y) ** 2;
      return distA - distB;
    });

    // Build hull using stack
    const hull = [];

    for (const p of sorted) {
      // Remove points that make clockwise turn
      while (hull.length > 1 && this.cross(hull[hull.length - 2], hull[hull.length - 1], p) <= 0) {
        hull.pop();
      }
      hull.push(p);
    }

    return hull;
  }

  /**
   * Cross product of vectors (B-A) and (C-A)
   * Positive = counter-clockwise, Negative = clockwise, Zero = collinear
   */
  cross(a, b, c) {
    return (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
  }

  /**
   * Extract hulls for multiple blobs
   *
   * @param {Array<Object>} blobs - Blobs from BlobDetector
   * @returns {Array<Object>} Array of hulls
   */
  extractAll(blobs) {
    const hulls = [];

    for (const blob of blobs) {
      const hull = this.extractHull(blob);

      if (hull.cartesianVertices.length >= 3) {
        hulls.push({
          blob,
          ...hull,
        });
      }
    }

    return hulls;
  }
}
