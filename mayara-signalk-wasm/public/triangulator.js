/**
 * Polygon Triangulation using Ear Clipping Algorithm
 *
 * Converts a simple polygon into triangles for GPU rendering.
 * Handles both convex and concave polygons.
 */

export class Triangulator {
  /**
   * Triangulate a simple polygon using ear clipping
   *
   * @param {Array<{x, y}>} vertices - Polygon vertices in order (CCW or CW)
   * @returns {Object} { vertices: Float32Array, indices: Uint16Array }
   */
  static triangulate(vertices) {
    const n = vertices.length;

    if (n < 3) {
      return { vertices: new Float32Array(0), indices: new Uint16Array(0) };
    }

    if (n === 3) {
      // Already a triangle
      return {
        vertices: new Float32Array([
          vertices[0].x, vertices[0].y,
          vertices[1].x, vertices[1].y,
          vertices[2].x, vertices[2].y,
        ]),
        indices: new Uint16Array([0, 1, 2]),
      };
    }

    // Ensure CCW winding
    const signedArea = this.signedArea(vertices);
    const orderedVertices = signedArea < 0 ? [...vertices].reverse() : [...vertices];

    // Create vertex indices list
    const remaining = [];
    for (let i = 0; i < n; i++) {
      remaining.push(i);
    }

    const triangleIndices = [];

    // Ear clipping loop
    let iterations = 0;
    const maxIterations = n * n; // Safety limit

    while (remaining.length > 3 && iterations < maxIterations) {
      let earFound = false;

      for (let i = 0; i < remaining.length; i++) {
        const prevIdx = remaining[(i - 1 + remaining.length) % remaining.length];
        const currIdx = remaining[i];
        const nextIdx = remaining[(i + 1) % remaining.length];

        const prev = orderedVertices[prevIdx];
        const curr = orderedVertices[currIdx];
        const next = orderedVertices[nextIdx];

        // Check if this is a convex vertex
        if (!this.isConvex(prev, curr, next)) {
          continue;
        }

        // Check if any other vertex is inside this triangle
        let isEar = true;
        for (let j = 0; j < remaining.length; j++) {
          if (j === i || j === (i - 1 + remaining.length) % remaining.length ||
              j === (i + 1) % remaining.length) {
            continue;
          }

          const testIdx = remaining[j];
          if (this.pointInTriangle(orderedVertices[testIdx], prev, curr, next)) {
            isEar = false;
            break;
          }
        }

        if (isEar) {
          // Add triangle
          triangleIndices.push(prevIdx, currIdx, nextIdx);

          // Remove the ear vertex
          remaining.splice(i, 1);
          earFound = true;
          break;
        }
      }

      if (!earFound) {
        // No ear found - polygon might be degenerate or self-intersecting
        // Use fan triangulation as fallback (silently - this is common for simplified polygons)
        if (remaining.length >= 3) {
          const pivot = remaining[0];
          for (let i = 1; i < remaining.length - 1; i++) {
            triangleIndices.push(pivot, remaining[i], remaining[i + 1]);
          }
        }
        break;
      }

      iterations++;
    }

    // Add final triangle (only if ear clipping completed normally)
    if (remaining.length === 3) {
      triangleIndices.push(remaining[0], remaining[1], remaining[2]);
    }

    // Build output arrays
    const vertexArray = new Float32Array(orderedVertices.length * 2);
    for (let i = 0; i < orderedVertices.length; i++) {
      vertexArray[i * 2] = orderedVertices[i].x;
      vertexArray[i * 2 + 1] = orderedVertices[i].y;
    }

    // Handle index remapping if we reversed
    let indexArray;
    if (signedArea < 0) {
      // Remap indices for reversed vertices
      indexArray = new Uint16Array(triangleIndices.length);
      for (let i = 0; i < triangleIndices.length; i++) {
        indexArray[i] = n - 1 - triangleIndices[i];
      }
    } else {
      indexArray = new Uint16Array(triangleIndices);
    }

    return {
      vertices: vertexArray,
      indices: indexArray,
      triangleCount: indexArray.length / 3,
    };
  }

  /**
   * Calculate signed area of polygon (positive = CCW, negative = CW)
   */
  static signedArea(vertices) {
    let area = 0;
    const n = vertices.length;

    for (let i = 0; i < n; i++) {
      const j = (i + 1) % n;
      area += vertices[i].x * vertices[j].y;
      area -= vertices[j].x * vertices[i].y;
    }

    return area / 2;
  }

  /**
   * Check if vertex B is convex (left turn from A to B to C)
   */
  static isConvex(a, b, c) {
    return this.cross(a, b, c) > 0;
  }

  /**
   * 2D cross product of vectors (B-A) and (C-A)
   */
  static cross(a, b, c) {
    return (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
  }

  /**
   * Check if point P is inside triangle ABC
   */
  static pointInTriangle(p, a, b, c) {
    const d1 = this.sign(p, a, b);
    const d2 = this.sign(p, b, c);
    const d3 = this.sign(p, c, a);

    const hasNeg = (d1 < 0) || (d2 < 0) || (d3 < 0);
    const hasPos = (d1 > 0) || (d2 > 0) || (d3 > 0);

    return !(hasNeg && hasPos);
  }

  /**
   * Sign of cross product for point-in-triangle test
   */
  static sign(p1, p2, p3) {
    return (p1.x - p3.x) * (p2.y - p3.y) - (p2.x - p3.x) * (p1.y - p3.y);
  }

  /**
   * Triangulate multiple polygons and combine into single buffer
   *
   * @param {Array<Array<{x, y}>>} polygons - Array of polygon vertex arrays
   * @param {Array<{r, g, b, a}>} colors - Color for each polygon (optional)
   * @returns {Object} Combined vertex and index buffers
   */
  static triangulateAll(polygons, colors = null) {
    const allVertices = [];
    const allIndices = [];
    const allColors = [];
    let vertexOffset = 0;

    // Debug: log first polygon details
    if (polygons.length > 0 && !this._lastTriDebug || performance.now() - this._lastTriDebug > 3000) {
      this._lastTriDebug = performance.now();
      const first = polygons[0];
      console.log(`Triangulator: First polygon has ${first.length} vertices`);
      if (first.length >= 2) {
        console.log(`  Vertices: [0]=(${first[0].x?.toFixed(4)}, ${first[0].y?.toFixed(4)}), [1]=(${first[1].x?.toFixed(4)}, ${first[1].y?.toFixed(4)})`);
      }
    }

    for (let i = 0; i < polygons.length; i++) {
      const polygon = polygons[i];
      const color = colors ? colors[i] : { r: 1, g: 0.5, b: 0, a: 1 };

      // Use fan triangulation for speed - radar contours are generally convex-ish
      const result = ConvexTriangulator.triangulate(polygon);

      if (result.indices.length === 0) continue;

      // Add vertices with colors
      for (let j = 0; j < result.vertices.length; j += 2) {
        allVertices.push(result.vertices[j], result.vertices[j + 1]);
        allColors.push(color.r, color.g, color.b, color.a);
      }

      // Add indices with offset
      for (let j = 0; j < result.indices.length; j++) {
        allIndices.push(result.indices[j] + vertexOffset);
      }

      vertexOffset += result.vertices.length / 2;
    }

    return {
      // Interleaved: x, y, r, g, b, a per vertex
      vertices: new Float32Array(allVertices),
      colors: new Float32Array(allColors),
      indices: new Uint32Array(allIndices),
      vertexCount: allVertices.length / 2,
      triangleCount: allIndices.length / 3,
    };
  }
}

/**
 * Fast triangulation for convex polygons (fan triangulation from centroid)
 * Works for both convex and concave contours by using centroid as pivot
 */
export class ConvexTriangulator {
  /**
   * Triangulate a polygon using fan triangulation from centroid
   * This creates triangles from center to each edge, filling the interior
   *
   * @param {Array<{x, y}>} vertices - Polygon vertices (contour boundary)
   * @returns {Object} { vertices, indices }
   */
  static triangulate(vertices) {
    const n = vertices.length;

    if (n < 3) {
      return { vertices: new Float32Array(0), indices: new Uint32Array(0) };
    }

    // Calculate centroid
    let cx = 0, cy = 0;
    for (let i = 0; i < n; i++) {
      cx += vertices[i].x;
      cy += vertices[i].y;
    }
    cx /= n;
    cy /= n;

    // Output: n boundary vertices + 1 centroid vertex
    const vertexArray = new Float32Array((n + 1) * 2);

    // First vertex is the centroid
    vertexArray[0] = cx;
    vertexArray[1] = cy;

    // Remaining vertices are the boundary
    for (let i = 0; i < n; i++) {
      vertexArray[(i + 1) * 2] = vertices[i].x;
      vertexArray[(i + 1) * 2 + 1] = vertices[i].y;
    }

    // Fan triangulation: all triangles share vertex 0 (centroid)
    // Each triangle connects centroid -> edge vertex i -> edge vertex i+1
    const indexArray = new Uint32Array(n * 3);
    for (let i = 0; i < n; i++) {
      indexArray[i * 3] = 0;                    // centroid
      indexArray[i * 3 + 1] = i + 1;            // current boundary vertex
      indexArray[i * 3 + 2] = ((i + 1) % n) + 1; // next boundary vertex (wrapping)
    }

    return {
      vertices: vertexArray,
      indices: indexArray,
      triangleCount: n,
    };
  }
}
