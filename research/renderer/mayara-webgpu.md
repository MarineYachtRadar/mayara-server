# Mayara WebGPU Renderer Analysis (render_webgpu.js)

This document analyzes the WebGPU-based renderer from Mayara, which uses texture-based polar-to-cartesian conversion similar to render_webgl.js but with the modern WebGPU API.

## Source File
- **Location**: `mayara-signalk-wasm/public/render_webgpu.js` (commit: 52639a5)
- **Lines**: ~290
- **API**: WebGPU (navigator.gpu)

---

## 1. Rendering Technique Overview

### Approach: WebGPU Texture-Based Rendering

Same conceptual approach as render_webgl.js:
1. Store radar data in a 2D texture (polar: angle × radius)
2. Draw fullscreen quad (4 vertices, TRIANGLE_STRIP)
3. Fragment shader converts cartesian to polar coordinates
4. Sample radar texture and color table for final color

### Key Differences from WebGL Version
- **Async initialization**: WebGPU requires await for device setup
- **Bind groups**: Resources bound via bind groups, not individual uniforms
- **WGSL shaders**: WebGPU Shading Language instead of GLSL
- **Command encoding**: Explicit command buffer recording

---

## 2. Async Initialization

### Constructor Pattern

```javascript
constructor(canvas_dom, canvas_background_dom, drawBackground) {
    this.ready = false;
    this.pendingLegend = null;
    this.pendingSpokes = null;

    // Start async initialization
    this.initPromise = this.#initWebGPU();
}

async #initWebGPU() {
    if (!navigator.gpu) {
        throw new Error("WebGPU not supported");
    }

    const adapter = await navigator.gpu.requestAdapter();
    const device = await adapter.requestDevice();
    const context = this.dom.getContext("webgpu");

    this.canvasFormat = navigator.gpu.getPreferredCanvasFormat();
    context.configure({
        device: device,
        format: this.canvasFormat,
        alphaMode: "premultiplied",
    });

    this.ready = true;
    // Apply any pending calls
    if (this.pendingSpokes) { ... }
    if (this.pendingLegend) { ... }
}
```

### Pending Call Pattern
Since setSpokes/setLegend may be called before WebGPU is ready, they queue pending operations:

```javascript
setSpokes(spokesPerRevolution, max_spoke_len) {
    if (!this.ready) {
        this.pendingSpokes = { spokesPerRevolution, max_spoke_len };
        // Still create CPU buffer for data accumulation
        this.data = new Uint8Array(spokesPerRevolution * max_spoke_len);
        return;
    }
    // ... normal setup
}
```

---

## 3. Resource Creation

### Polar Data Texture

```javascript
this.polarTexture = this.device.createTexture({
    size: [max_spoke_len, spokesPerRevolution],
    format: "r8unorm",  // 8-bit unsigned normalized [0,1]
    usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
});
```

### Color Table Texture

```javascript
this.colorTexture = this.device.createTexture({
    size: [256, 1],
    format: "rgba8unorm",
    usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
});

// Upload color data
const colorTableData = new Uint8Array(256 * 4);
for (let i = 0; i < l.length; i++) {
    colorTableData[i * 4] = l[i][0];      // R
    colorTableData[i * 4 + 1] = l[i][1];  // G
    colorTableData[i * 4 + 2] = l[i][2];  // B
    colorTableData[i * 4 + 3] = l[i][3];  // A
}

this.device.queue.writeTexture(
    { texture: this.colorTexture },
    colorTableData,
    { bytesPerRow: 256 * 4 },
    { width: 256, height: 1 }
);
```

### Sampler

```javascript
this.sampler = this.device.createSampler({
    magFilter: "linear",
    minFilter: "linear",
    addressModeU: "clamp-to-edge",
    addressModeV: "clamp-to-edge",
});
```

### Uniform Buffer (Transformation Matrix)

```javascript
this.uniformBuffer = this.device.createBuffer({
    size: 64,  // 4x4 matrix = 16 floats = 64 bytes
    usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
});
```

### Vertex Buffer

```javascript
const vertices = new Float32Array([
    // Position (x, y), TexCoord (u, v)
    -1.0, -1.0, 0.0, 0.0,
     1.0, -1.0, 1.0, 0.0,
    -1.0,  1.0, 0.0, 1.0,
     1.0,  1.0, 1.0, 1.0,
]);

this.vertexBuffer = this.device.createBuffer({
    size: vertices.byteLength,
    usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
});
this.device.queue.writeBuffer(this.vertexBuffer, 0, vertices);
```

---

## 4. Bind Group Layout

```javascript
const bindGroupLayout = this.device.createBindGroupLayout({
    entries: [
        { binding: 0, visibility: GPUShaderStage.FRAGMENT,
          texture: { sampleType: "float" } },           // polar data
        { binding: 1, visibility: GPUShaderStage.FRAGMENT,
          texture: { sampleType: "float" } },           // color table
        { binding: 2, visibility: GPUShaderStage.FRAGMENT,
          sampler: { type: "filtering" } },             // sampler
        { binding: 3, visibility: GPUShaderStage.VERTEX,
          buffer: { type: "uniform" } },                // transform matrix
    ],
});

this.bindGroup = this.device.createBindGroup({
    layout: bindGroupLayout,
    entries: [
        { binding: 0, resource: this.polarTexture.createView() },
        { binding: 1, resource: this.colorTexture.createView() },
        { binding: 2, resource: this.sampler },
        { binding: 3, resource: { buffer: this.uniformBuffer } },
    ],
});
```

---

## 5. Render Pipeline

```javascript
this.pipeline = this.device.createRenderPipeline({
    layout: pipelineLayout,
    vertex: {
        module: this.shaderModule,
        entryPoint: "vertexMain",
        buffers: [{
            arrayStride: 16,  // 4 floats × 4 bytes
            attributes: [
                { shaderLocation: 0, offset: 0, format: "float32x2" },  // position
                { shaderLocation: 1, offset: 8, format: "float32x2" },  // texCoord
            ],
        }],
    },
    fragment: {
        module: this.shaderModule,
        entryPoint: "fragmentMain",
        targets: [{ format: this.canvasFormat }],
    },
    primitive: { topology: "triangle-strip" },
});
```

---

## 6. WGSL Shaders

### Vertex Shader

```wgsl
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) texCoord: vec2<f32>,
}

@group(0) @binding(3) var<uniform> u_transform: mat4x4<f32>;

@vertex
fn vertexMain(
    @location(0) pos: vec2<f32>,
    @location(1) texCoord: vec2<f32>
) -> VertexOutput {
    var output: VertexOutput;
    output.position = u_transform * vec4<f32>(pos, 0.0, 1.0);
    output.texCoord = texCoord;
    return output;
}
```

### Fragment Shader

```wgsl
@group(0) @binding(0) var polarData: texture_2d<f32>;
@group(0) @binding(1) var colorTable: texture_2d<f32>;
@group(0) @binding(2) var texSampler: sampler;

@fragment
fn fragmentMain(@location(0) texCoord: vec2<f32>) -> @location(0) vec4<f32> {
    // Convert cartesian to polar
    let centered = texCoord - vec2<f32>(0.5, 0.5);
    let r = length(centered) * 2.0;
    let theta = atan2(centered.y, centered.x);

    // Normalize theta to [0, 1]
    let normalizedTheta = 1.0 - (theta + 3.14159265) / (2.0 * 3.14159265);

    // Sample radar data (index 0-1)
    let index = textureSample(polarData, texSampler, vec2<f32>(r, normalizedTheta)).r;

    // Color table lookup
    let color = textureSample(colorTable, texSampler, vec2<f32>(index, 0.0));

    // Mask outside circle and empty data
    let insideCircle = step(r, 1.0);
    let hasData = step(0.004, index);  // ~1/255 threshold
    let alpha = insideCircle * hasData * color.a;

    return vec4<f32>(color.rgb, alpha);
}
```

### Key Shader Features
- **step() functions**: Avoids if-statements for better GPU performance
- **Masking**: Transparent outside radar circle and for empty data
- **Same math**: Identical polar conversion as WebGL version

---

## 7. Rendering

### drawSpoke() - Accumulate Data

```javascript
drawSpoke(spoke) {
    if (!this.data) return;

    let offset = spoke.angle * this.max_spoke_len;
    this.data.set(spoke.data, offset);
    if (spoke.data.length < this.max_spoke_len) {
        this.data.fill(0, offset + spoke.data.length, offset + this.max_spoke_len);
    }
}
```

### render() - GPU Upload and Draw

```javascript
render() {
    if (!this.ready || !this.data || !this.pipeline) return;

    // Upload spoke data to GPU texture
    this.device.queue.writeTexture(
        { texture: this.polarTexture },
        this.data,
        { bytesPerRow: this.max_spoke_len },
        { width: this.max_spoke_len, height: this.spokesPerRevolution }
    );

    // Create command encoder
    const encoder = this.device.createCommandEncoder();

    // Begin render pass
    const pass = encoder.beginRenderPass({
        colorAttachments: [{
            view: this.context.getCurrentTexture().createView(),
            clearValue: { r: 0.0, g: 0.0, b: 0.0, a: 0.0 },
            loadOp: "clear",
            storeOp: "store",
        }],
    });

    // Draw
    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, this.bindGroup);
    pass.setVertexBuffer(0, this.vertexBuffer);
    pass.draw(4);
    pass.end();

    // Submit
    this.device.queue.submit([encoder.finish()]);
}
```

---

## 8. Transformation Matrix

```javascript
#setTransformationMatrix() {
    const range = this.range || this.actual_range || 1500;
    const scale = (1.0 * this.actual_range) / range;
    const angle = Math.PI / 2;  // 90° rotation

    const scaleX = scale * ((2 * this.beam_length) / this.width);
    const scaleY = scale * ((2 * this.beam_length) / this.height);

    const cos = Math.cos(angle);
    const sin = Math.sin(angle);

    // Combined rotation + scaling (column-major for WebGPU)
    const transformMatrix = new Float32Array([
        cos * scaleX, -sin * scaleX, 0.0, 0.0,
        sin * scaleY,  cos * scaleY, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
    ]);

    this.device.queue.writeBuffer(this.uniformBuffer, 0, transformMatrix);
}
```

---

## 9. Pros and Cons

### Advantages
1. **Modern API**: WebGPU is the future of web graphics
2. **Better performance potential**: More explicit control
3. **Compute shader ready**: Could add GPU preprocessing
4. **Cross-platform**: Same code works on all WebGPU implementations

### Disadvantages
1. **Browser support**: Not yet universal (Chrome/Edge only as of 2024)
2. **Complexity**: More boilerplate than WebGL
3. **Async setup**: Requires careful handling of initialization
4. **No fallback**: Must detect and use WebGL if unavailable

---

## 10. WebGPU vs WebGL Comparison

| Aspect | WebGPU | WebGL2 |
|--------|--------|--------|
| API style | Modern, explicit | Legacy OpenGL |
| Initialization | Async (await) | Sync |
| Resource binding | Bind groups | Individual uniforms |
| Shader language | WGSL | GLSL ES |
| Command submission | Command encoder | Immediate mode |
| Browser support | Chrome, Edge | Universal |
| Texture upload | queue.writeTexture | texImage2D |

---

## 11. Error Handling

```javascript
async #initWebGPU() {
    if (!navigator.gpu) {
        throw new Error("WebGPU not supported");
    }

    const adapter = await navigator.gpu.requestAdapter();
    if (!adapter) {
        throw new Error("No WebGPU adapter found");
    }

    this.device = await adapter.requestDevice();
    // ...
}
```

Caller should catch these errors and fall back to WebGL.

---

## 12. Canvas Configuration

```javascript
// Initial setup
this.context.configure({
    device: this.device,
    format: this.canvasFormat,
    alphaMode: "premultiplied",
});

// On resize (in redrawCanvas)
this.context.configure({
    device: this.device,
    format: this.canvasFormat,
    alphaMode: "premultiplied",
});
```

**alphaMode: "premultiplied"**: Enables transparency, important for overlaying radar on charts.

---

## 13. Memory Layout

### Texture Memory

| Texture | Format | Size | Bytes |
|---------|--------|------|-------|
| Polar data | r8unorm | 1024 × 2048 | 2 MB |
| Color table | rgba8unorm | 256 × 1 | 1 KB |
| **Total** | | | ~2 MB |

### Buffer Memory

| Buffer | Size | Purpose |
|--------|------|---------|
| Uniform | 64 bytes | Transform matrix |
| Vertex | 64 bytes | Fullscreen quad |
| **Total** | 128 bytes | |

---

## 14. Source Code Reference

**File**: `mayara-signalk-wasm/public/render_webgpu.js` (commit 52639a5)

Key functions:
- `constructor()` - Start async init
- `#initWebGPU()` - Device setup, pipeline creation
- `#createPipelineAndBindGroup()` - WebGPU pipeline setup
- `setSpokes()` - Create polar texture, handle pending
- `setLegend()` - Create color table texture
- `drawSpoke()` - Copy spoke data to CPU buffer
- `render()` - Texture upload, command encoding, submit
- `#setTransformationMatrix()` - Update uniform buffer

---

## 15. Usage in viewer.js

```javascript
// Detection and fallback
try {
    if (draw == "webgpu") {
        renderer = new render_webgpu(canvas, background, drawBackground);
    }
} catch (e) {
    console.log("WebGPU not available, falling back to WebGL");
    renderer = new render_webgl(canvas, background, drawBackground);
}
```

The viewer should detect WebGPU support and fall back gracefully.
