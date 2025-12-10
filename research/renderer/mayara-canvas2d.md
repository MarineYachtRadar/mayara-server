# Mayara Canvas 2D Renderer Analysis (render_2d.js)

This document analyzes the Canvas 2D fallback renderer from Mayara's `main` branch.

## Source File
- **Location**: `mayara-server/web/render_2d.js` (branch: `main`)
- **Lines**: ~100
- **API**: Canvas 2D (`getContext("2d")`)

---

## 1. Rendering Technique Overview

### Approach: Arc Wedges with Pattern Fill

This renderer uses the Canvas 2D API to draw radar spokes as filled arc wedges:
1. Create a 1-pixel-high pattern containing the spoke's color data
2. Transform the canvas to align with the spoke angle
3. Draw an arc wedge and fill with the pattern

```
     Pattern (1px height, spoke_len width)
     ┌────────────────────────────────────┐
     │ color[0] │ color[1] │ ... │ color[n] │
     └────────────────────────────────────┘
              ↓ (repeated as fill)
           ╱─────╲
          ╱   arc  ╲
         ╱  wedge   ╲
        ●───────────●
           center
```

---

## 2. Initialization

### Constructor

```javascript
constructor(canvas_dom, canvas_background_dom, drawBackground) {
    this.dom = canvas_dom;
    this.background_dom = canvas_background_dom;
    this.drawBackgroundCallback = drawBackground;
    this.redrawCanvas();
}
```

### redrawCanvas() - Setup

```javascript
redrawCanvas() {
    // Get dimensions from parent
    var parent = this.dom.parentNode,
        styles = getComputedStyle(parent),
        w = parseInt(styles.getPropertyValue("width"), 10),
        h = parseInt(styles.getPropertyValue("height"), 10);

    this.dom.width = w;
    this.dom.height = h;
    this.background_dom.width = w;
    this.background_dom.height = h;

    this.width = this.dom.width;
    this.height = this.dom.height;
    this.center_x = this.width / 2;
    this.center_y = this.height / 2;
    this.beam_length = Math.trunc(
        Math.max(this.center_x, this.center_y) * RANGE_SCALE
    );

    // Get 2D context
    this.ctx = this.dom.getContext("2d", { alpha: true });
    this.background_ctx = this.background_dom.getContext("2d");

    // Create pattern canvas (1 pixel high, 2048 wide)
    this.pattern = document.createElement("canvas");
    this.pattern.width = 2048;
    this.pattern.height = 1;
    this.pattern_ctx = this.pattern.getContext("2d");
    this.image = this.pattern_ctx.createImageData(2048, 1);

    this.drawBackgroundCallback(this, "MAYARA (Canvas 2D)");
}
```

---

## 3. Spoke Drawing

### drawSpoke() - The Core Algorithm

```javascript
drawSpoke(spoke) {
    // Calculate angle (shift by 270° so angle 0 = up/bow)
    let a = (2 * Math.PI *
        ((spoke.angle + (this.spokesPerRevolution * 3) / 4) % this.spokesPerRevolution)
    ) / this.spokesPerRevolution;

    // Calculate pixels per radar sample
    let pixels_per_item = (this.beam_length * RANGE_SCALE) / spoke.data.length;
    if (this.range) {
        pixels_per_item = (pixels_per_item * spoke.range) / this.range;
    }

    // Pre-compute transform components
    let c = Math.cos(a) * pixels_per_item;
    let s = Math.sin(a) * pixels_per_item;

    // Fill pattern image with spoke colors
    for (let i = 0, idx = 0; i < spoke.data.length; i++, idx += 4) {
        let v = spoke.data[i];
        this.image.data[idx + 0] = this.legend[v][0];  // R
        this.image.data[idx + 1] = this.legend[v][1];  // G
        this.image.data[idx + 2] = this.legend[v][2];  // B
        this.image.data[idx + 3] = this.legend[v][3];  // A
    }

    // Put image data into pattern canvas
    this.pattern_ctx.putImageData(this.image, 0, 0);

    // Create repeating pattern
    let pattern = this.ctx.createPattern(this.pattern, "repeat-x");

    // Arc angle for one spoke width
    let arc_angle = (2 * Math.PI) / this.spokesPerRevolution;

    // Transform: rotate and scale to spoke position
    this.ctx.setTransform(c, s, -s, c, this.center_x, this.center_y);

    // Draw filled arc wedge
    this.ctx.fillStyle = pattern;
    this.ctx.beginPath();
    this.ctx.moveTo(0, 0);
    this.ctx.arc(0, 0, spoke.data.length, 0, arc_angle);
    this.ctx.closePath();
    this.ctx.fill();
}
```

### Step-by-Step Breakdown

1. **Angle calculation**: Convert spoke angle to radians, offset by 270° so 0 = up
2. **Scale factor**: Calculate how many pixels per radar sample
3. **Pattern creation**: Fill ImageData with RGBA colors from legend
4. **Transform**: Use `setTransform()` to rotate/scale canvas
5. **Arc drawing**: Draw wedge from center with `arc()` and `fill()`

---

## 4. The Transform Matrix

### setTransform(a, b, c, d, e, f)

The Canvas 2D transform matrix:
```
| a  c  e |
| b  d  f |
| 0  0  1 |
```

In drawSpoke():
```javascript
let c = Math.cos(a) * pixels_per_item;
let s = Math.sin(a) * pixels_per_item;
this.ctx.setTransform(c, s, -s, c, this.center_x, this.center_y);
```

This creates:
```
| cos(a)*scale  -sin(a)*scale  center_x |
| sin(a)*scale   cos(a)*scale  center_y |
| 0              0              1        |
```

Which combines:
- **Rotation** by angle `a`
- **Scaling** by `pixels_per_item`
- **Translation** to canvas center

---

## 5. Pattern Fill Technique

### Why Pattern Fill?

Instead of drawing thousands of individual pixels or lines, this technique:
1. Creates a 1-pixel-high image with all colors in a row
2. Uses `createPattern("repeat-x")` to tile it horizontally
3. The pattern stretches radially when filling the arc

### Pattern Canvas

```javascript
this.pattern = document.createElement("canvas");
this.pattern.width = 2048;  // Max spoke length
this.pattern.height = 1;    // Single pixel row
this.image = this.pattern_ctx.createImageData(2048, 1);
```

Each spoke:
1. Writes colors to `this.image.data` (RGBA array)
2. Calls `putImageData()` to update pattern canvas
3. Creates new pattern from canvas
4. Fills arc with pattern

---

## 6. Simple Interface

### setSpokes()
```javascript
setSpokes(spokesPerRevolution, max_spoke_len) {
    this.spokesPerRevolution = spokesPerRevolution;
    this.max_spoke_len = max_spoke_len;
}
```

### setRange()
```javascript
setRange(range) {
    this.range = range;
    this.redrawCanvas();
}
```

### setLegend()
```javascript
setLegend(l) {
    this.legend = l;  // Store as-is (0-255 RGBA arrays)
}
```

### render()
```javascript
render() {
    // Empty! All drawing happens in drawSpoke()
}
```

---

## 7. Angle Conversion

### Why +270° (3/4 revolution)?

```javascript
(spoke.angle + (this.spokesPerRevolution * 3) / 4) % this.spokesPerRevolution
```

- Canvas 2D arc starts at 3 o'clock (East, angle=0)
- Radar angle 0 should point up (North, 12 o'clock)
- Offset by 270° (3/4 turn) to rotate coordinate system

```
Canvas default:        Radar convention:
      90°                    0° (bow)
       │                      │
180° ──●── 0°          270° ──●── 90°
       │                      │
      270°                  180°
```

---

## 8. Pros and Cons

### Advantages
1. **Universal support**: Canvas 2D works everywhere
2. **Simple code**: No shaders, no GPU setup
3. **Fallback**: Works when WebGL unavailable
4. **Easy debugging**: Standard DOM canvas

### Disadvantages
1. **CPU-bound**: All rendering on main thread
2. **Slower**: Pattern creation per spoke is expensive
3. **No GPU acceleration**: Can't leverage modern hardware
4. **Memory churn**: Creates new pattern each spoke

---

## 9. Performance Characteristics

### Per-Spoke Cost
- `putImageData()`: Copy ~8KB (2048×4 bytes)
- `createPattern()`: Browser overhead
- `arc()` + `fill()`: Path rendering

### Typical Performance
- Adequate for low refresh rates
- May struggle with high spoke counts
- CPU usage scales with spoke rate

---

## 10. Comparison with Other Renderers

| Aspect | Canvas 2D | WebGL | WebGL Alt | WebGPU |
|--------|-----------|-------|-----------|--------|
| API | `getContext("2d")` | WebGL2 | WebGL2 | WebGPU |
| Rendering | CPU | GPU (texture) | GPU (geometry) | GPU (texture) |
| Draw calls | Per spoke | 1 per batch | 1 per batch | 1 per batch |
| Pattern/Texture | Per-spoke pattern | Static texture | None | Static texture |
| Complexity | Low | Medium | Medium | High |
| Performance | Lowest | High | High | Highest |
| Support | Universal | ~98% | ~98% | ~70% |

---

## 11. When to Use Canvas 2D

- **Fallback**: When WebGL/WebGPU unavailable
- **Debugging**: Easier to inspect/modify
- **Low-end devices**: May have better driver support
- **Simple displays**: When performance isn't critical

---

## 12. Source Code Reference

**File**: `mayara-server/web/render_2d.js` (main branch)

Key functions:
- `constructor()` - Basic setup
- `redrawCanvas()` - Create contexts and pattern canvas
- `setSpokes()` - Store spoke parameters
- `setLegend()` - Store color legend
- `setRange()` - Store range, trigger redraw
- `drawSpoke()` - Core rendering (pattern + arc)
- `render()` - Empty (drawing is immediate)

---

## 13. Code Flow Summary

```
setSpokes() → Store spokesPerRevolution, max_spoke_len
setLegend() → Store color array
setRange()  → Store range, redrawCanvas()

For each spoke:
  drawSpoke() →
    1. Calculate angle and scale
    2. Fill image.data with colors
    3. putImageData to pattern canvas
    4. createPattern from canvas
    5. setTransform to rotate/scale
    6. Draw arc wedge with pattern fill

render() → (nothing, already drawn)
```
