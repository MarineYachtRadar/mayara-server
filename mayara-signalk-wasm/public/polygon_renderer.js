/**
 * WebGPU Polygon Renderer
 *
 * Renders filled polygons (as triangles) extracted from radar data.
 * Uses dynamic vertex/index buffers that can be updated each frame.
 */

export class PolygonRenderer {
  /**
   * @param {GPUDevice} device - WebGPU device
   * @param {GPUCanvasContext} context - Canvas context
   * @param {string} canvasFormat - Preferred canvas format
   */
  constructor(device, context, canvasFormat) {
    this.device = device;
    this.context = context;
    this.canvasFormat = canvasFormat;

    // Maximum vertices/indices we can handle
    // Radar can have 1000+ blobs with ~100 vertices each = ~150k vertices
    this.maxVertices = 200000;
    this.maxIndices = 200000 * 3;

    // Current counts
    this.vertexCount = 0;
    this.indexCount = 0;

    this.#initBuffers();
    this.#initPipeline();
  }

  #initBuffers() {
    // Vertex buffer: position (x, y) + color (r, g, b, a) = 6 floats per vertex
    this.vertexBuffer = this.device.createBuffer({
      size: this.maxVertices * 6 * 4, // 6 floats * 4 bytes
      usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
    });

    // Index buffer (Uint32 to support >65536 vertices)
    this.indexBuffer = this.device.createBuffer({
      size: this.maxIndices * 4, // Uint32
      usage: GPUBufferUsage.INDEX | GPUBufferUsage.COPY_DST,
    });

    // Uniform buffer for transformation matrix
    this.uniformBuffer = this.device.createBuffer({
      size: 64, // 4x4 matrix
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });
  }

  #initPipeline() {
    const shaderModule = this.device.createShaderModule({
      code: polygonShaderCode,
    });

    const bindGroupLayout = this.device.createBindGroupLayout({
      entries: [
        {
          binding: 0,
          visibility: GPUShaderStage.VERTEX,
          buffer: { type: "uniform" },
        },
      ],
    });

    this.bindGroup = this.device.createBindGroup({
      layout: bindGroupLayout,
      entries: [
        { binding: 0, resource: { buffer: this.uniformBuffer } },
      ],
    });

    this.pipeline = this.device.createRenderPipeline({
      layout: this.device.createPipelineLayout({
        bindGroupLayouts: [bindGroupLayout],
      }),
      vertex: {
        module: shaderModule,
        entryPoint: "vertexMain",
        buffers: [
          {
            arrayStride: 24, // 6 floats * 4 bytes
            attributes: [
              { shaderLocation: 0, offset: 0, format: "float32x2" },  // position
              { shaderLocation: 1, offset: 8, format: "float32x4" },  // color
            ],
          },
        ],
      },
      fragment: {
        module: shaderModule,
        entryPoint: "fragmentMain",
        targets: [{
          format: this.canvasFormat,
          blend: {
            // For premultiplied alpha: src is already multiplied by alpha
            // So use "one" for srcFactor (not "src-alpha")
            color: {
              srcFactor: "one",
              dstFactor: "one-minus-src-alpha",
              operation: "add",
            },
            alpha: {
              srcFactor: "one",
              dstFactor: "one-minus-src-alpha",
              operation: "add",
            },
          },
        }],
      },
      primitive: {
        topology: "triangle-list",
        cullMode: "none", // Don't cull - polygons might have mixed winding
      },
    });
  }

  /**
   * Update the transformation matrix
   *
   * @param {Float32Array} matrix - 4x4 transformation matrix (column-major)
   */
  setTransform(matrix) {
    this.device.queue.writeBuffer(this.uniformBuffer, 0, matrix);
  }

  /**
   * Update polygon data from triangulated polygons
   *
   * @param {Array<{x, y}>} vertices - Flat array of vertex positions
   * @param {Array<{r, g, b, a}>} colors - Color per vertex
   * @param {Array<number>} indices - Triangle indices
   */
  updatePolygons(vertices, colors, indices) {
    if (vertices.length === 0 || indices.length === 0) {
      this.vertexCount = 0;
      this.indexCount = 0;
      return;
    }

    // Clamp to max
    const numVertices = Math.min(vertices.length / 2, this.maxVertices);
    const numIndices = Math.min(indices.length, this.maxIndices);

    // Build interleaved vertex data: x, y, r, g, b, a
    const vertexData = new Float32Array(numVertices * 6);
    for (let i = 0; i < numVertices; i++) {
      const vi = i * 6;
      vertexData[vi] = vertices[i * 2];
      vertexData[vi + 1] = vertices[i * 2 + 1];
      vertexData[vi + 2] = colors[i * 4];
      vertexData[vi + 3] = colors[i * 4 + 1];
      vertexData[vi + 4] = colors[i * 4 + 2];
      vertexData[vi + 5] = colors[i * 4 + 3];
    }

    // Upload to GPU (use Uint32 for indices to support >65536 vertices)
    this.device.queue.writeBuffer(this.vertexBuffer, 0, vertexData);
    this.device.queue.writeBuffer(this.indexBuffer, 0, new Uint32Array(indices.slice(0, numIndices)));

    this.vertexCount = numVertices;
    this.indexCount = numIndices;
  }

  /**
   * Update from combined triangulation result
   *
   * @param {Object} triangulationResult - Result from Triangulator.triangulateAll()
   */
  updateFromTriangulation(triangulationResult) {
    if (!triangulationResult || triangulationResult.vertexCount === 0) {
      this.vertexCount = 0;
      this.indexCount = 0;
      return;
    }

    const { vertices, colors, indices } = triangulationResult;

    // Debug: log sample vertex data periodically
    if (!this._lastVertexLog || performance.now() - this._lastVertexLog > 3000) {
      this._lastVertexLog = performance.now();
      const numVerts = vertices.length / 2;
      const numIndices = indices.length;

      if (vertices.length >= 4) {
        console.log(`PolygonRenderer: ${numVerts} vertices, ${numIndices} indices (${numIndices / 3} triangles)`);
        console.log(`  Vertex sample: [0]=(${vertices[0].toFixed(4)}, ${vertices[1].toFixed(4)}), [1]=(${vertices[2].toFixed(4)}, ${vertices[3].toFixed(4)})`);

        // Check vertex ranges
        let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
        for (let i = 0; i < vertices.length; i += 2) {
          minX = Math.min(minX, vertices[i]);
          maxX = Math.max(maxX, vertices[i]);
          minY = Math.min(minY, vertices[i + 1]);
          maxY = Math.max(maxY, vertices[i + 1]);
        }
        console.log(`  Vertex bounds: X=[${minX.toFixed(4)}, ${maxX.toFixed(4)}], Y=[${minY.toFixed(4)}, ${maxY.toFixed(4)}]`);

        // Validate indices
        let maxIndex = 0, invalidCount = 0;
        for (let i = 0; i < indices.length; i++) {
          if (indices[i] >= numVerts) invalidCount++;
          maxIndex = Math.max(maxIndex, indices[i]);
        }
        console.log(`  Index range: 0-${maxIndex}, invalid indices: ${invalidCount}`);
      }
      if (colors.length >= 4) {
        console.log(`  Color sample: RGBA=(${colors[0].toFixed(2)}, ${colors[1].toFixed(2)}, ${colors[2].toFixed(2)}, ${colors[3].toFixed(2)})`);
      }
    }

    this.updatePolygons(vertices, colors, indices);
  }

  /**
   * Render the polygons
   *
   * @param {GPUCommandEncoder} encoder - Command encoder to use
   * @param {GPUTextureView} targetView - Render target view
   * @param {boolean} clear - Whether to clear the target first
   */
  render(encoder, targetView, clear = false) {
    if (this.indexCount === 0) return;

    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view: targetView,
          clearValue: { r: 0, g: 0, b: 0, a: 0 },
          loadOp: clear ? "clear" : "load",
          storeOp: "store",
        },
      ],
    });

    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, this.bindGroup);
    pass.setVertexBuffer(0, this.vertexBuffer);
    pass.setIndexBuffer(this.indexBuffer, "uint32");
    pass.drawIndexed(this.indexCount);
    pass.end();
  }

  /**
   * Standalone render (creates own encoder)
   */
  renderStandalone() {
    if (this.indexCount === 0) {
      // Debug: log when no indices
      if (!this._lastNoIndexLog || performance.now() - this._lastNoIndexLog > 2000) {
        this._lastNoIndexLog = performance.now();
        console.log('PolygonRenderer: No indices to render');
      }
      return;
    }

    // Debug: log render stats periodically
    if (!this._lastRenderLog || performance.now() - this._lastRenderLog > 2000) {
      this._lastRenderLog = performance.now();
      const tex = this.context.getCurrentTexture();
      console.log(`PolygonRenderer: Rendering ${this.indexCount} indices (${this.indexCount / 3} triangles), ${this.vertexCount} vertices`);
      console.log(`  Canvas texture: ${tex.width}x${tex.height}, format=${tex.format}`);
    }

    const encoder = this.device.createCommandEncoder();
    const targetView = this.context.getCurrentTexture().createView();

    this.render(encoder, targetView, true);

    this.device.queue.submit([encoder.finish()]);
  }

  /**
   * Debug: Render a test triangle to verify pipeline works
   */
  renderTestTriangle() {
    // Create a simple triangle that fills most of the screen
    const testVertices = new Float32Array([
      // x, y, r, g, b, a
      0.0, 0.8, 1.0, 0.0, 0.0, 1.0,   // top center - red
     -0.8, -0.8, 0.0, 1.0, 0.0, 1.0,  // bottom left - green
      0.8, -0.8, 0.0, 0.0, 1.0, 1.0,  // bottom right - blue
    ]);
    // Pad to 4 indices (8 bytes) for WebGPU alignment requirement
    const testIndices = new Uint16Array([0, 1, 2, 0]);

    this.device.queue.writeBuffer(this.vertexBuffer, 0, testVertices);
    this.device.queue.writeBuffer(this.indexBuffer, 0, testIndices);
    this.vertexCount = 3;
    this.indexCount = 3;  // Only draw 3 indices, the 4th is padding

    // Use identity transform
    const identity = new Float32Array([
      1, 0, 0, 0,
      0, 1, 0, 0,
      0, 0, 1, 0,
      0, 0, 0, 1,
    ]);
    this.device.queue.writeBuffer(this.uniformBuffer, 0, identity);

    const tex = this.context.getCurrentTexture();
    console.log(`PolygonRenderer: Rendering TEST TRIANGLE to ${tex.width}x${tex.height} canvas`);

    const encoder = this.device.createCommandEncoder();
    const targetView = tex.createView();
    this.render(encoder, targetView, true);
    this.device.queue.submit([encoder.finish()]);
  }
}

/**
 * Simple polygon shader
 */
const polygonShaderCode = `
struct VertexOutput {
  @builtin(position) position: vec4<f32>,
  @location(0) color: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u_transform: mat4x4<f32>;

@vertex
fn vertexMain(
  @location(0) pos: vec2<f32>,
  @location(1) color: vec4<f32>
) -> VertexOutput {
  var output: VertexOutput;
  // Apply transformation - vertices are in [-1, 1] Cartesian space
  output.position = u_transform * vec4<f32>(pos, 0.0, 1.0);
  // Pass through vertex color
  output.color = color;
  return output;
}

@fragment
fn fragmentMain(@location(0) color: vec4<f32>) -> @location(0) vec4<f32> {
  // Premultiplied alpha output
  return vec4<f32>(color.rgb * color.a, color.a);
}
`;
