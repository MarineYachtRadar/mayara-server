# SignalK-Radar Rendering Analysis

This document analyzes how the signalk-radar project (Go backend + freeboard-sk frontend) renders radar images.

## Project Overview

- **Backend**: `/home/dirk/dev/signalk-radar/radar-server/` (Go)
- **Frontend**: `/home/dirk/dev/freeboard-sk/` branch `radar-support` (TypeScript/Angular)
- **Supported Radars**: Navico (2048 spokes), Garmin XHD (1440 spokes)

---

## 1. Data Pipeline

### Backend (Go) Data Flow

```
UDP/Multicast Frame → Parse Header → Unpack Pixels → Protobuf Message → WebSocket
```

### Radar Constants

| Radar | Spokes | Max Spoke Length |
|-------|--------|------------------|
| Navico (BR24/3G/4G/HALO) | 2048 | 1024 |
| Garmin XHD | 1440 | 705 |

### Pixel Unpacking (Navico)

Navico radars pack **2 pixels per byte** (nibbles). The Go code uses lookup tables:

```go
// From navico.go:430-435
for i := 0; i < NAVICO_MAX_SPOKE_LEN/2; i++ {
    data_highres[2*i] = g.pixelToBlob[lookupIndex(lowNibbleIndex, int(data[i]))]
    data_highres[2*i+1] = g.pixelToBlob[lookupIndex(highNibbleIndex, int(data[i]))]
}
```

Special values for Doppler mode:
- `0x0f` → DopplerApproaching (cyan)
- `0x0e` → DopplerReceding (light cyan)

---

## 2. Color Legend System

### Legend Generation (Go)

**File**: `/home/dirk/dev/signalk-radar/radar-server/radar/radar.go:118-167`

The default legend creates a **Blue → Green → Red** gradient based on signal strength:

```go
func DefaultLegend(doppler bool, pixelValues int) Legend {
    // pixelValues = 16 for Navico (values 0-15)

    const WHITE float32 = 255
    pixelsWithColor := pixelValues - 1  // 15
    start := WHITE / 3.0                 // 85
    delta := WHITE * 2.0 / float32(pixelsWithColor)  // ~34
    oneThird := pixelsWithColor / 3      // 5
    twoThirds := oneThird * 2            // 10
```

### Color Mapping Table

| Pixel Value | Range | Color | Purpose |
|-------------|-------|-------|---------|
| 0 | - | `#00000000` | Transparent/empty |
| 1-5 | 0 to 1/3 | Blue gradient | Weak returns |
| 6-10 | 1/3 to 2/3 | Green gradient | Medium returns |
| 11-15 | 2/3 to max | Red gradient | Strong returns |
| 16 | Border | `#C8C8C8FF` (gray) | Target border |
| 17 | Doppler Approaching | `#00C8C8FF` (cyan) | Moving toward |
| 18 | Doppler Receding | `#90D0F0FF` (light cyan) | Moving away |
| 19-50 | History | Grayscale gradient | Trail history |

### Color Calculation Formula

```go
// Blue region (v < oneThird):
b = start + v * (WHITE / pixelValues)  // ~85 + v*17

// Green region (oneThird <= v < twoThirds):
g = start + (v - oneThird) * delta     // ~85 + (v-5)*34

// Red region (v >= twoThirds):
r = start + (v - twoThirds) * delta    // ~85 + (v-10)*34
```

---

## 3. WebGL Rendering Algorithm

### Overview

The renderer uses **WebGL2 with GL_LINES** primitive to draw radar spokes.

**File**: `freeboard-sk/src/app/modules/map/ol/lib/radar/radar-gl.worker.ts` (radar-support branch)

### Pre-calculated Coordinate Grid

At initialization, a coordinate lookup table is built for all possible spoke positions:

```typescript
// Build coordinate grid (unit circle, normalized -1 to 1)
const cx = 0, cy = 0
const maxRadius = 1
const angleShift = ((2 * Math.PI) / radar.spokes) / 2  // Half-spoke offset

for (let a = 0; a < radar.spokes; a++) {
    for (let r = 0; r < radar.maxSpokeLen; r++) {
        const angle = (a * ((2 * Math.PI) / radar.spokes)) + angleShift
        const radius = r * (maxRadius / radar.maxSpokeLen)
        const x1 = cx + ((radius + radiusShift) * Math.cos(angle))
        const y1 = cy + ((radius + radiusShift) * Math.sin(angle))
        x[a * radar.maxSpokeLen + r] = x1
        y[a * radar.maxSpokeLen + r] = -y1  // Flip Y for screen coords
    }
}
```

**Key insight**: The `angleShift` offsets each spoke by half a spoke-width, centering the lines in their angular sector.

### Spoke Drawing Method: LINE PAIRS

For each spoke, the renderer draws **lines connecting the current spoke to the next spoke** at each radius:

```typescript
let spokeBearing = ToBearing(spoke.angle)
let ba = spokeBearing + 1  // Next spoke index
if (ba > (radar.spokes) - 1) {
    ba = 0  // Wrap around
}

for (let i = 0; i < spoke.data.length; i++) {
    // Vertex 1: Current spoke at radius i
    vertices.push(x[spokeBearing * radar.maxSpokeLen + i])
    vertices.push(y[spokeBearing * radar.maxSpokeLen + i])
    vertices.push(0.0)

    // Vertex 2: Next spoke at radius i
    vertices.push(x[ba * radar.maxSpokeLen + i])
    vertices.push(y[ba * radar.maxSpokeLen + i])
    vertices.push(0.0)

    // Color from legend (same color for both vertices)
    let color = colors.get(spoke.data[i])
    verticeColors.push(color[0]/255, color[1]/255, color[2]/255, color[3])
    verticeColors.push(color[0]/255, color[1]/255, color[2]/255, color[3])
}
```

### Visual Representation

```
           Spoke N+1
          /
         /  Line at radius r
        /
       *---------* ← Spoke N
      /|        /|
     / |       / |
    *--|------*  | ← Lines at each radius
       |         |
       Center
```

Each line connects `(spoke_n, radius_r)` to `(spoke_n+1, radius_r)`, creating **arc-like segments** that fill the wedge between adjacent spokes.

### WebGL Draw Call

```typescript
radarContext.drawArrays(radarContext.LINES, 0, vertices.length / 3);
```

---

## 4. Shader Code

### Vertex Shader

```glsl
attribute vec3 coordinates;
attribute vec4 color;
varying vec4 vColor;

void main(void) {
    gl_Position = vec4(coordinates, 1.0);
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

**Note**: Very simple pass-through shaders. No transformations, no texture sampling. All coordinate transformation is done in JavaScript before uploading to GPU.

---

## 5. Rendering Context Setup

```typescript
const radarContext = canvas.getContext("webgl2", {
    preserveDrawingBuffer: true  // Important: preserves content between frames
});

// Transparent background
radarContext.clearColor(0.0, 0.0, 0.0, 0.0);
```

---

## 6. Key Differences from Mayara

| Aspect | SignalK-Radar | Mayara (current) |
|--------|---------------|------------------|
| Spoke count | 2048 (Navico native) | 2048 (reduced from 8192) |
| Pixel values | 0-15 (4-bit) | 0-255 (8-bit) |
| Drawing primitive | `GL_LINES` | `GL_LINES` (alt), Texture (webgl) |
| Color scheme | Blue→Green→Red | Green→Yellow→Red |
| Coordinate system | Unit circle (-1 to 1) | Unit circle with transform matrix |
| Clear behavior | Clear on range change | Clear on range change |

---

## 7. Why GL_LINES Works

With 2048 spokes covering 360°:
- Angular resolution: 360° / 2048 = 0.176° per spoke
- At outer edge of a 1024-pixel radius: arc length ≈ 3.14 pixels per spoke

The lines connecting adjacent spokes at each radius create a **dense mesh** that appears solid because:
1. Lines are drawn for every radius sample (1024 lines per spoke)
2. Adjacent lines overlap slightly at inner radii
3. At outer radii, the angular gap is still < ~3 pixels

---

## 8. Recommendations for Mayara

### Option A: Use Same GL_LINES Approach
Keep the current `render_webgl_alt.js` approach but ensure:
1. Pre-calculate coordinates with proper `angleShift` offset
2. Draw lines from current spoke to next spoke (not individual spoke lines)
3. Use consistent vertex buffer binding

### Option B: Use GL_TRIANGLE_STRIP (Better Fill)
For each spoke, emit vertices alternating between current and next spoke:
```
v0 (spoke_n, r=0)
v1 (spoke_n+1, r=0)
v2 (spoke_n, r=1)
v3 (spoke_n+1, r=1)
...
```
Use degenerate triangles to separate spokes.

### Option C: Texture-Based Rendering
Keep the `render_webgl.js` approach but:
1. Ensure proper texture alignment (UNPACK_ALIGNMENT = 1)
2. Consider using NEAREST filtering to avoid color bleeding
3. The texture approach works best with high spoke counts (8192)

---

## 9. Source Files Reference

### Backend (Go)
- `/home/dirk/dev/signalk-radar/radar-server/radar/radar.go` - Legend, color generation
- `/home/dirk/dev/signalk-radar/radar-server/radar/navico/navico.go` - Navico protocol, pixel unpacking
- `/home/dirk/dev/signalk-radar/radar-server/radar/schema/RadarMessage.proto` - Protobuf schema

### Frontend (TypeScript) - branch: radar-support
- `src/app/modules/map/ol/lib/radar/radar-gl.worker.ts` - WebGL renderer
- `src/app/modules/map/ol/lib/radar/radar.worker.ts` - Canvas 2D fallback
- `src/app/modules/radar/skresources/resource-classes.ts` - SKRadar data model

---

## 10. Conclusion

The signalk-radar project uses a straightforward **GL_LINES** approach where each radar sample creates a line segment connecting adjacent spokes at the same radius. This creates a dense mesh of lines that visually fills the radar display. The key to making it work is:

1. **Pre-calculated coordinates** for all possible spoke/radius combinations
2. **Line pairs** connecting adjacent spokes (not individual radial lines)
3. **Simple shaders** with color passed per-vertex
4. **Proper angleShift** to center lines in their angular sector

The approach is efficient and produces good visual results with 2048 spokes.
