# Mayara WebGL Alt Renderer Analysis (render_webgl_alt.js)

This document analyzes the geometry-based WebGL renderer from Mayara's `main` branch, which uses GL_LINES similar to signalk-radar.

## Source File
- **Location**: `mayara-server/web/render_webgl_alt.js` (branch: `main`)
- **Lines**: ~280
- **API**: WebGL2 (with WebGL1-style shaders)

---

## 1. Rendering Technique Overview

### Approach: Geometry-Based Line Drawing

This renderer uses the same technique as signalk-radar:
1. Pre-calculates a coordinate grid for all spoke/radius positions
2. For each spoke, generates LINE vertices connecting to the next spoke
3. Colors are assigned per-vertex from the legend
4. GPU draws lines; density creates solid appearance

```
        Spoke N+1
       /
      /  Line at radius r
     /
    *---------* ← Spoke N
   /         /
  /         /
 *---------*  ← Lines fill the wedge
    ↑
  Center
```

---

## 2. Coordinate Pre-calculation

### setSpokes() - Grid Generation

```javascript
setSpokes(spokesPerRevolution, max_spoke_len) {
    let x = [];
    let y = [];
    const cx = 0, cy = 0;
    const maxRadius = 1;
    const angleShift = (2 * Math.PI) / this.spokesPerRevolution / 2;

    for (let a = 0; a < this.spokesPerRevolution; a++) {
        for (let r = 0; r < this.max_spoke_len; r++) {
            const angle = a * ((2 * Math.PI) / this.spokesPerRevolution) + angleShift;
            const radius = r * (maxRadius / this.max_spoke_len);
            const x1 = cx + radius * Math.cos(angle);
            const y1 = cy + radius * Math.sin(angle);
            x[a * this.max_spoke_len + r] = x1;
            y[a * this.max_spoke_len + r] = -y1;  // Flip Y for screen coords
        }
    }
    this.x = x;
    this.y = y;
}
```

### Key Parameters

| Parameter | Formula | Purpose |
|-----------|---------|---------|
| angleShift | `(2π / spokes) / 2` | Centers lines in angular sector |
| radius | `r / max_spoke_len` | Normalizes to [0, 1] |
| Y flip | `-y1` | Converts math coords to screen coords |

### Memory Usage
For 2048 spokes × 1024 radius:
- `x[]`: 2,097,152 floats = 8 MB
- `y[]`: 2,097,152 floats = 8 MB
- Total: **16 MB** pre-computed coordinates

---

## 3. Shaders

### Vertex Shader (WebGL1 style)
```glsl
attribute vec3 coordinates;
attribute vec4 color;
uniform mat4 u_transform;
varying vec4 vColor;

void main(void) {
    gl_Position = u_transform * vec4(coordinates, 1.0);
    vColor = color;
}
```

### Fragment Shader
```glsl
precision mediump float;
varying vec4 vColor;

void main(void) {
    gl_FragColor = vColor;
}
```

**Note**: Very simple pass-through shaders. All coordinate computation is done in JavaScript.

---

## 4. Spoke Drawing

### drawSpoke() - Generate Vertices

```javascript
drawSpoke(spoke) {
    let spokeBearing = spoke.has_bearing
        ? spoke.bearing
        : this.#angleToBearing(spoke.angle);

    let ba = spokeBearing + 1;  // Next spoke index
    if (ba > this.spokesPerRevolution - 1) {
        ba = 0;  // Wrap around
    }

    let offset = spokeBearing * this.max_spoke_len;
    let next_offset = ba * this.max_spoke_len;

    for (let i = 0; i < spoke.data.length; i++) {
        // Vertex 1: Current spoke at radius i
        this.vertices.push(this.x[offset + i]);
        this.vertices.push(this.y[offset + i]);
        this.vertices.push(0.0);

        // Vertex 2: Next spoke at radius i
        this.vertices.push(this.x[next_offset + i]);
        this.vertices.push(this.y[next_offset + i]);
        this.vertices.push(0.0);

        // Color for both vertices
        let color = this.legend[spoke.data[i]];
        if (color) {
            this.verticeColors.push(color[0], color[1], color[2], color[3]);
            this.verticeColors.push(color[0], color[1], color[2], color[3]);
        } else {
            // Fallback: transparent white
            this.verticeColors.push(1.0, 1.0, 1.0, 0);
            this.verticeColors.push(1.0, 1.0, 1.0, 0);
        }
    }
}
```

### Line Topology

For each radius sample in the spoke:
- **Vertex A**: `(x[spoke_n, radius_r], y[spoke_n, radius_r], 0)`
- **Vertex B**: `(x[spoke_n+1, radius_r], y[spoke_n+1, radius_r], 0)`

This creates a line segment that spans the angular gap between adjacent spokes.

---

## 5. Render Pipeline

### render() - Upload and Draw

```javascript
render() {
    let gl = this.gl;

    // Upload vertex positions
    gl.bindBuffer(gl.ARRAY_BUFFER, this.vertexBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(this.vertices), gl.STATIC_DRAW);

    // Upload colors
    gl.bindBuffer(gl.ARRAY_BUFFER, this.colorBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(this.verticeColors), gl.STATIC_DRAW);

    // Draw all lines
    gl.bindBuffer(gl.ARRAY_BUFFER, null);
    gl.drawArrays(gl.LINES, 0, this.vertices.length / 3);

    // Clear for next frame
    this.vertices = [];
    this.verticeColors = [];
}
```

### Per-Frame Memory

For a batch of 32 spokes × 1024 radius samples:
- Vertices: 32 × 1024 × 2 × 3 floats = 196,608 floats = 786 KB
- Colors: 32 × 1024 × 2 × 4 floats = 262,144 floats = 1 MB
- **Total per batch**: ~1.8 MB

---

## 6. Transformation Matrix

### Construction (Scaling Only)

```javascript
#setTransformationMatrix() {
    let scale = (RANGE_SCALE * this.actual_range) / this.range;

    this.transform_matrix = new Float32Array([
        scale * ((2 * this.beam_length) / this.width), 0, 0, 0,
        0, scale * ((2 * this.beam_length) / this.height), 0, 0,
        0, 0, 1, 0,
        0, 0, 0, 1,
    ]);

    this.gl.uniformMatrix4fv(this.transform_matrix_location, false, this.transform_matrix);
}
```

**Note**: Unlike `render_webgl.js`, there's NO rotation matrix. The 90° rotation is baked into the coordinate pre-calculation via the Y-flip and angle conventions.

---

## 7. Color Legend System

### setLegend() - Scale to OpenGL Range

```javascript
setLegend(l) {
    let a = Array();
    for (let i = 0; i < Object.keys(l).length; i++) {
        let color = l[i];
        color[0] = color[0] / 255;  // Scale 0-255 to 0-1
        color[1] = color[1] / 255;
        color[2] = color[2] / 255;
        color[3] = color[3] / 255;
        a.push(color);
    }
    this.legend = a;
}
```

Colors are stored as float arrays [r, g, b, a] where each component is in range [0.0, 1.0].

---

## 8. Heading Support

### angleToBearing() - Heading Adjustment

```javascript
#angleToBearing(angle) {
    let h = this.heading - 90;
    if (h < 0) {
        h += 360;
    }
    angle += Math.round(h / (360 / this.spokesPerRevolution));
    angle = angle % this.spokesPerRevolution;
    return angle;
}
```

This converts radar angles (relative to bow) to screen bearings (accounting for vessel heading).

---

## 9. Context Setup

```javascript
let gl = this.dom.getContext("webgl2", {
    preserveDrawingBuffer: true,  // Keep content between frames
});

gl.clearColor(0.0, 0.0, 0.0, 0.0);  // Transparent background
gl.clear(gl.COLOR_BUFFER_BIT);
```

**preserveDrawingBuffer**: Required because spokes accumulate over time; we don't redraw all spokes each frame.

---

## 10. Known Issue in Original Code

### Bug: Attribute Binding After Buffer Switch

The original `render()` function has a potential bug:

```javascript
// PROBLEMATIC CODE:
gl.bindBuffer(gl.ARRAY_BUFFER, this.vertexBuffer);
gl.bufferData(...);
gl.bindBuffer(gl.ARRAY_BUFFER, this.colorBuffer);
gl.bufferData(...);
gl.bindBuffer(gl.ARRAY_BUFFER, null);  // Unbinds!
gl.drawArrays(gl.LINES, ...);  // Draws with wrong bindings
```

After switching between vertex and color buffers, the vertex attribute pointers may point to the wrong buffer. Fix:

```javascript
// Upload vertex positions and rebind attribute
gl.bindBuffer(gl.ARRAY_BUFFER, this.vertexBuffer);
gl.bufferData(...);
gl.vertexAttribPointer(this.coordAttr, 3, gl.FLOAT, false, 0, 0);

// Upload colors and rebind attribute
gl.bindBuffer(gl.ARRAY_BUFFER, this.colorBuffer);
gl.bufferData(...);
gl.vertexAttribPointer(this.colorAttr, 4, gl.FLOAT, false, 0, 0);

// Draw
gl.drawArrays(gl.LINES, 0, this.vertices.length / 3);
```

---

## 11. Pros and Cons

### Advantages
1. **Direct control**: Each line explicitly placed
2. **No shader math**: Simple pass-through fragment shader
3. **Discrete values**: No interpolation artifacts
4. **Same as signalk-radar**: Proven technique

### Disadvantages
1. **High vertex count**: ~65K vertices per spoke batch
2. **CPU work per frame**: JavaScript vertex generation
3. **Memory allocation**: New Float32Array each render()
4. **No interpolation**: Discrete lines may show gaps

---

## 12. Comparison with render_webgl.js

| Aspect | render_webgl_alt.js | render_webgl.js |
|--------|---------------------|-----------------|
| Technique | Geometry (GL_LINES) | Texture + shader |
| Vertices | ~200K per batch | 4 total |
| CPU work | High (vertex gen) | Low (memcpy) |
| GPU work | Low (simple shader) | High (polar math) |
| Memory | 16MB static + dynamic | Fixed texture |
| Interpolation | None | LINEAR filtering |
| Precision | Discrete | Continuous |

---

## 13. Visual Result

With 2048 spokes:
- Angular resolution: 360° / 2048 = 0.176° per spoke
- At 1024-pixel radius: arc length ≈ 3.1 pixels per spoke
- Lines overlap sufficiently to appear solid

The GL_LINES approach works because the high spoke count creates sufficient line density to fill the display.

---

## 14. Source Code Reference

**File**: `mayara-server/web/render_webgl_alt.js` (main branch)

Key functions:
- `constructor()` - WebGL2 setup with WebGL1-style shaders
- `setSpokes()` - Pre-calculate coordinate grid
- `setLegend()` - Store colors scaled to [0,1]
- `#angleToBearing()` - Heading adjustment
- `drawSpoke()` - Generate line vertices for spoke
- `render()` - Upload buffers and draw
- `#setTransformationMatrix()` - Scaling matrix (no rotation)
