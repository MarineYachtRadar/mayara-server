# Mayara WebGL Renderer Analysis (render_webgl.js)

This document analyzes the texture-based WebGL renderer from Mayara's `main` branch.

## Source File
- **Location**: `mayara-server/web/render_webgl.js` (branch: `main`)
- **Lines**: ~330
- **API**: WebGL2

---

## 1. Rendering Technique Overview

### Approach: Texture-Based Polar-to-Cartesian Conversion

Unlike geometry-based approaches (GL_LINES), this renderer:
1. Stores radar data in a 2D texture (polar coordinates: angle × radius)
2. Draws a fullscreen quad (4 vertices, TRIANGLE_STRIP)
3. Fragment shader converts each pixel from cartesian to polar coordinates
4. Samples the radar texture and color table to produce final color

```
┌─────────────────────────────┐
│     Fragment Shader         │
│  pixel(x,y) → polar(r,θ)   │
│       ↓                     │
│  texture sample at (r,θ)    │
│       ↓                     │
│  color table lookup         │
│       ↓                     │
│  final pixel color          │
└─────────────────────────────┘
```

---

## 2. Data Structures

### Polar Data Texture
- **Format**: `R8` (single-channel, 8-bit unsigned)
- **Dimensions**: `max_spoke_len × spokesPerRevolution`
- **Layout**: Each row = one spoke, each column = radius sample
- **Values**: 0-255 (color index)

```javascript
// Create texture data buffer
let data = new Uint8Array(spokesPerRevolution * max_spoke_len);

// Upload to GPU each frame
gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8,
    max_spoke_len, spokesPerRevolution, 0,
    gl.RED, gl.UNSIGNED_BYTE, data);
```

### Color Table Texture
- **Format**: `RGBA8` (4 channels, 8-bit each)
- **Dimensions**: `256 × 1` (1D lookup table)
- **Purpose**: Maps radar value (0-255) to RGBA color

```javascript
const colorTableData = new Uint8Array(256 * 4);
for (let i = 0; i < l.length; i++) {
    colorTableData[i * 4] = l[i][0];     // Red
    colorTableData[i * 4 + 1] = l[i][1]; // Green
    colorTableData[i * 4 + 2] = l[i][2]; // Blue
    colorTableData[i * 4 + 3] = l[i][3]; // Alpha
}
```

---

## 3. Vertex Shader

```glsl
#version 300 es
in vec4 a_position;
in vec2 a_texCoord;
out vec2 v_texCoord;

uniform mat4 u_transform;

void main() {
    gl_Position = u_transform * a_position;
    v_texCoord = a_texCoord;
}
```

### Vertex Buffer (Fullscreen Quad)
```javascript
const positions = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
const texCoords = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
```

---

## 4. Fragment Shader

```glsl
#version 300 es
precision mediump float;

in vec2 v_texCoord;
out vec4 color;

uniform sampler2D u_polarIndexData;
uniform sampler2D u_colorTable;

void main() {
    // Convert texture coordinates into polar coordinates
    vec2 centeredCoords = v_texCoord - vec2(0.5, 0.5);
    float r = length(centeredCoords) * 2.0;
    float theta = atan(centeredCoords.y, centeredCoords.x);

    // Normalize theta to [0, 1] range
    float normalizedTheta = 1.0 - (theta + 3.14159265) / (2.0 * 3.14159265);

    // Sample polar data texture
    float index = texture(u_polarIndexData, vec2(r, normalizedTheta)).r;

    // Look up color from table
    color = texture(u_colorTable, vec2(index, 0.0));
}
```

### Polar Coordinate Conversion

| Step | Formula | Result |
|------|---------|--------|
| Center coords | `v_texCoord - 0.5` | Range: [-0.5, 0.5] |
| Radius | `length(centered) * 2.0` | Range: [0, ~1.414] |
| Angle | `atan2(y, x)` | Range: [-π, π] |
| Normalize θ | `1 - (θ + π) / (2π)` | Range: [0, 1] |

### Texture Sampling
- `r` maps to U coordinate (radius → horizontal)
- `normalizedTheta` maps to V coordinate (angle → vertical)
- Result is the radar index (0.0 - 1.0, representing 0-255)

---

## 5. Transformation Matrix

### Construction
```javascript
const scale = (1.0 * this.actual_range) / this.range;
const angle = Math.PI / 2;  // 90° rotation

// Scaling matrix
const scaling_matrix = new Float32Array([
    scale * ((2 * this.beam_length) / this.width), 0, 0, 0,
    0, scale * ((2 * this.beam_length) / this.height), 0, 0,
    0, 0, 1, 0,
    0, 0, 0, 1,
]);

// Rotation matrix (90° around Z-axis)
const rotation_matrix = new Float32Array([
    cos(angle), -sin(angle), 0, 0,
    sin(angle),  cos(angle), 0, 0,
    0, 0, 1, 0,
    0, 0, 0, 1,
]);

// Combined: scaling × rotation
multiply(transformation_matrix, scaling_matrix, rotation_matrix);
```

### Purpose
- **Scaling**: Fits radar circle to canvas size, accounts for range zoom
- **Rotation**: Rotates 90° so angle 0 (bow) points up instead of right

---

## 6. Texture Filtering

```javascript
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
```

- **LINEAR filtering**: Interpolates between adjacent spokes for smooth appearance
- **CLAMP_TO_EDGE**: Prevents texture wrapping artifacts at edges

---

## 7. Rendering Pipeline

### Per-Spoke Update (drawSpoke)
```javascript
drawSpoke(spoke) {
    let offset = spoke.angle * this.max_spoke_len;
    this.data.set(spoke.data, offset);
    if (spoke.data.length < this.max_spoke_len) {
        this.data.fill(0, offset + spoke.data.length, offset + this.max_spoke_len);
    }
}
```

### Batch Render (render)
```javascript
render() {
    updateTexture(gl, this.data, this.spokesPerRevolution, this.max_spoke_len);
    draw(gl);
}

function draw(gl) {
    gl.clearColor(0.1, 0.3, 0.1, 1.0);  // Dark green background
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.viewport(0, 0, gl.canvas.width, gl.canvas.height);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
}
```

---

## 8. Pros and Cons

### Advantages
1. **GPU-efficient**: Only 4 vertices, all work in fragment shader
2. **Smooth interpolation**: LINEAR filtering creates smooth gradients
3. **Flexible zoom**: Matrix transformation handles range scaling
4. **Low CPU overhead**: Just memcpy spoke data to buffer

### Disadvantages
1. **Per-pixel math**: Fragment shader runs for every screen pixel
2. **No partial updates**: Full texture upload each render()
3. **Angle wrap issues**: May have artifacts at θ=0/2π boundary
4. **Fixed resolution**: Texture resolution determines quality ceiling

---

## 9. Key Parameters

| Parameter | Value | Description |
|-----------|-------|-------------|
| RANGE_SCALE | 0.9 | Fill factor for radar circle |
| Background | (0.1, 0.3, 0.1) | Dark green |
| Texture Format | R8 | 8-bit single channel |
| Draw Mode | TRIANGLE_STRIP | 4 vertices = 2 triangles |

---

## 10. Comparison with signalk-radar

| Aspect | render_webgl.js | signalk-radar |
|--------|-----------------|---------------|
| Technique | Texture + shader | Geometry (GL_LINES) |
| Vertex count | 4 | spoke_len × spokes × 2 |
| Fragment complexity | High (polar math) | Low (pass-through) |
| Memory | Fixed texture size | Variable (vertex arrays) |
| Interpolation | Built-in (LINEAR) | None (discrete lines) |
| Spoke updates | Array copy | Push vertices |

---

## 11. Source Code Reference

**File**: `mayara-server/web/render_webgl.js` (main branch)

Key functions:
- `constructor()` - WebGL2 setup, shader compilation
- `setSpokes()` - Initialize texture dimensions
- `setLegend()` - Create color table texture
- `drawSpoke()` - Copy spoke data to CPU buffer
- `render()` - Upload texture, draw quad
- `#setTransformationMatrix()` - Compute scale/rotation matrix
