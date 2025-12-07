export { render_webgpu };

import { RANGE_SCALE, formatRangeValue, is_metric } from "./viewer.js";
import { BlobDetector } from "./blob_detector.js";
import { ContourTracer, PolygonSimplifier, ConvexHullExtractor, SectorExtractor } from "./contour_tracer.js";
import { Triangulator } from "./triangulator.js";
import { PolygonRenderer } from "./polygon_renderer.js";

class render_webgpu {
  // The constructor gets two canvases, the real drawing one and one for background data
  // such as range circles etc.
  constructor(canvas_dom, canvas_background_dom, drawBackground) {
    this.dom = canvas_dom;
    this.background_dom = canvas_background_dom;
    this.background_ctx = this.background_dom.getContext("2d");
    this.drawBackgroundCallback = drawBackground;

    this.actual_range = 0;
    this.ready = false;
    this.pendingLegend = null;
    this.pendingSpokes = null;

    // Tunable settings (can be adjusted via settings panel)
    this.settings = {
      transparencyThreshold: 11,  // Values below this are transparent (0-255)
      fillMultiplier: 3.0,        // Angular fill multiplier (increased for better patch connection)
      radialFill: 4.0,            // Radial fill expansion (increased for better patch connection)
      spokeCount: 2048,           // Number of spokes
      interpolation: 0.3,         // 0=max only, 1=mix+max
      gamma: 1.6,                 // Color gamma correction
      edgeSoftness: 0.06,         // Edge anti-aliasing width
      sampleCount: 10,            // Number of angular samples (increased for more fill)
      // Polygon mode settings
      usePolygons: true,          // Enable polygon-based rendering
      blobThreshold: 15,          // Threshold for blob detection
      minBlobSize: 30,            // Minimum blob size in pixels
      simplifyTolerance: 0.001,   // Douglas-Peucker tolerance (lower = more detail)
    };

    // Polygon processing state
    this.blobDetector = null;
    this.contourTracer = null;
    this.hullExtractor = null;
    this.sectorExtractor = null;
    this.polygonRenderer = null;
    this.lastPolygonUpdate = 0;
    this.polygonUpdateInterval = 50; // ms between polygon updates

    // Create settings panel
    this.#createSettingsPanel();

    // Start async initialization
    this.initPromise = this.#initWebGPU();
  }

  #createSettingsPanel() {
    const controller = document.getElementById("myr_controller");
    if (!controller) return;

    const settingsDiv = document.createElement("div");
    settingsDiv.id = "myr_webgpu_settings";
    settingsDiv.innerHTML = `
      <div style="margin-top: 20px; padding: 10px; border-top: 1px solid #555;">
        <div style="font-weight: bold; margin-bottom: 10px; color: #0f0;">WebGPU Settings</div>

        <label style="display: block; margin: 5px 0; color: #ccc; font-size: 12px;">
          Transparency: <span id="thresh_val">${this.settings.transparencyThreshold}</span>
          <input type="range" id="webgpu_threshold" min="0" max="50" value="${this.settings.transparencyThreshold}" style="width: 100%;">
        </label>

        <label style="display: block; margin: 5px 0; color: #ccc; font-size: 12px;">
          Angular Fill: <span id="fill_val">${this.settings.fillMultiplier.toFixed(1)}</span>
          <input type="range" id="webgpu_fill" min="0" max="200" value="${this.settings.fillMultiplier * 10}" style="width: 100%;">
        </label>

        <label style="display: block; margin: 5px 0; color: #ccc; font-size: 12px;">
          Radial Fill: <span id="radial_val">${this.settings.radialFill.toFixed(1)}</span>
          <input type="range" id="webgpu_radial" min="0" max="100" value="${this.settings.radialFill * 10}" style="width: 100%;">
        </label>

        <label style="display: block; margin: 5px 0; color: #ccc; font-size: 12px;">
          Samples: <span id="samples_val">${this.settings.sampleCount}</span>
          <input type="range" id="webgpu_samples" min="1" max="15" value="${this.settings.sampleCount}" style="width: 100%;">
        </label>

        <label style="display: block; margin: 5px 0; color: #ccc; font-size: 12px;">
          Interpolation: <span id="interp_val">${this.settings.interpolation.toFixed(1)}</span>
          <input type="range" id="webgpu_interp" min="0" max="10" value="${this.settings.interpolation * 10}" style="width: 100%;">
        </label>

        <label style="display: block; margin: 5px 0; color: #ccc; font-size: 12px;">
          Gamma: <span id="gamma_val">${this.settings.gamma.toFixed(1)}</span>
          <input type="range" id="webgpu_gamma" min="1" max="30" value="${this.settings.gamma * 10}" style="width: 100%;">
        </label>

        <label style="display: block; margin: 5px 0; color: #ccc; font-size: 12px;">
          Edge Soft: <span id="edge_val">${this.settings.edgeSoftness.toFixed(2)}</span>
          <input type="range" id="webgpu_edge" min="0" max="20" value="${this.settings.edgeSoftness * 100}" style="width: 100%;">
        </label>

        <div style="margin-top: 15px; padding-top: 10px; border-top: 1px solid #444;">
          <div style="font-weight: bold; margin-bottom: 10px; color: #0ff;">Polygon Mode</div>

          <label style="display: block; margin: 5px 0; color: #ccc; font-size: 12px;">
            <input type="checkbox" id="webgpu_use_polygons" ${this.settings.usePolygons ? 'checked' : ''}>
            Enable Polygon Rendering
          </label>

          <label style="display: block; margin: 5px 0; color: #ccc; font-size: 12px;">
            Blob Threshold: <span id="blob_thresh_val">${this.settings.blobThreshold}</span>
            <input type="range" id="webgpu_blob_threshold" min="1" max="100" value="${this.settings.blobThreshold}" style="width: 100%;">
          </label>

          <label style="display: block; margin: 5px 0; color: #ccc; font-size: 12px;">
            Min Blob Size: <span id="min_blob_val">${this.settings.minBlobSize}</span>
            <input type="range" id="webgpu_min_blob" min="5" max="200" value="${this.settings.minBlobSize}" style="width: 100%;">
          </label>

          <label style="display: block; margin: 5px 0; color: #ccc; font-size: 12px;">
            Simplify: <span id="simplify_val">${this.settings.simplifyTolerance.toFixed(4)}</span>
            <input type="range" id="webgpu_simplify" min="1" max="100" value="${this.settings.simplifyTolerance * 10000}" style="width: 100%;">
          </label>
        </div>
      </div>
    `;
    controller.appendChild(settingsDiv);

    // Event listeners for all sliders
    document.getElementById("webgpu_threshold").addEventListener("input", (e) => {
      this.settings.transparencyThreshold = parseInt(e.target.value);
      document.getElementById("thresh_val").textContent = this.settings.transparencyThreshold;
      this.#updateSettingsBuffer();
    });

    document.getElementById("webgpu_fill").addEventListener("input", (e) => {
      this.settings.fillMultiplier = parseInt(e.target.value) / 10;
      document.getElementById("fill_val").textContent = this.settings.fillMultiplier.toFixed(1);
      this.#updateSettingsBuffer();
    });

    document.getElementById("webgpu_radial").addEventListener("input", (e) => {
      this.settings.radialFill = parseInt(e.target.value) / 10;
      document.getElementById("radial_val").textContent = this.settings.radialFill.toFixed(1);
      this.#updateSettingsBuffer();
    });

    document.getElementById("webgpu_samples").addEventListener("input", (e) => {
      this.settings.sampleCount = parseInt(e.target.value);
      document.getElementById("samples_val").textContent = this.settings.sampleCount;
      this.#updateSettingsBuffer();
    });

    document.getElementById("webgpu_interp").addEventListener("input", (e) => {
      this.settings.interpolation = parseInt(e.target.value) / 10;
      document.getElementById("interp_val").textContent = this.settings.interpolation.toFixed(1);
      this.#updateSettingsBuffer();
    });

    document.getElementById("webgpu_gamma").addEventListener("input", (e) => {
      this.settings.gamma = parseInt(e.target.value) / 10;
      document.getElementById("gamma_val").textContent = this.settings.gamma.toFixed(1);
      this.#updateSettingsBuffer();
    });

    document.getElementById("webgpu_edge").addEventListener("input", (e) => {
      this.settings.edgeSoftness = parseInt(e.target.value) / 100;
      document.getElementById("edge_val").textContent = this.settings.edgeSoftness.toFixed(2);
      this.#updateSettingsBuffer();
    });

    // Polygon mode event listeners
    document.getElementById("webgpu_use_polygons").addEventListener("change", (e) => {
      this.settings.usePolygons = e.target.checked;
    });

    document.getElementById("webgpu_blob_threshold").addEventListener("input", (e) => {
      this.settings.blobThreshold = parseInt(e.target.value);
      document.getElementById("blob_thresh_val").textContent = this.settings.blobThreshold;
    });

    document.getElementById("webgpu_min_blob").addEventListener("input", (e) => {
      this.settings.minBlobSize = parseInt(e.target.value);
      document.getElementById("min_blob_val").textContent = this.settings.minBlobSize;
    });

    document.getElementById("webgpu_simplify").addEventListener("input", (e) => {
      this.settings.simplifyTolerance = parseInt(e.target.value) / 10000;
      document.getElementById("simplify_val").textContent = this.settings.simplifyTolerance.toFixed(4);
    });
  }

  #updateSettingsBuffer() {
    if (!this.ready || !this.settingsBuffer) return;
    const settingsData = new Float32Array([
      this.settings.transparencyThreshold / 255.0,
      this.settings.fillMultiplier,
      this.settings.radialFill,
      this.settings.sampleCount,
      this.settings.interpolation,
      this.settings.gamma,
      this.settings.edgeSoftness,
      this.spokesPerRevolution || 2048.0  // Actual spoke count (was padding)
    ]);
    this.device.queue.writeBuffer(this.settingsBuffer, 0, settingsData);
  }

  async #initWebGPU() {
    if (!navigator.gpu) {
      throw new Error("WebGPU not supported");
    }

    const adapter = await navigator.gpu.requestAdapter();
    if (!adapter) {
      throw new Error("No WebGPU adapter found");
    }

    this.device = await adapter.requestDevice();
    this.context = this.dom.getContext("webgpu");

    this.canvasFormat = navigator.gpu.getPreferredCanvasFormat();
    this.context.configure({
      device: this.device,
      format: this.canvasFormat,
      alphaMode: "premultiplied",
    });

    // Create shader module
    this.shaderModule = this.device.createShaderModule({
      code: shaderCode,
    });

    // Create sampler
    this.sampler = this.device.createSampler({
      magFilter: "linear",
      minFilter: "linear",
      addressModeU: "clamp-to-edge",
      addressModeV: "clamp-to-edge",
    });

    // Create uniform buffer for transformation matrix
    this.uniformBuffer = this.device.createBuffer({
      size: 64, // 4x4 matrix = 16 floats = 64 bytes
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });

    // Create settings buffer for shader parameters
    this.settingsBuffer = this.device.createBuffer({
      size: 32, // 8 floats = 32 bytes
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });
    this.#updateSettingsBuffer();

    // Create vertex buffer for fullscreen quad
    // TexCoords match WebGL: [0,0], [1,0], [0,1], [1,1]
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

    // Initialize polygon renderer
    this.polygonRenderer = new PolygonRenderer(this.device, this.context, this.canvasFormat);

    this.ready = true;
    this.redrawCanvas();

    // Apply pending calls if they were made before init completed
    if (this.pendingSpokes) {
      this.setSpokes(this.pendingSpokes.spokesPerRevolution, this.pendingSpokes.max_spoke_len);
      this.pendingSpokes = null;
    }
    if (this.pendingLegend) {
      this.setLegend(this.pendingLegend);
      this.pendingLegend = null;
    }
    console.log("WebGPU initialized successfully");
  }

  // This is called as soon as it is clear what the number of spokes and their max length is
  setSpokes(spokesPerRevolution, max_spoke_len) {
    console.log("WebGPU setSpokes:", spokesPerRevolution, max_spoke_len, "ready:", this.ready);

    if (!this.ready) {
      this.pendingSpokes = { spokesPerRevolution, max_spoke_len };
      // Still create CPU buffer for data accumulation
      this.spokesPerRevolution = spokesPerRevolution;
      this.max_spoke_len = max_spoke_len;
      this.data = new Uint8Array(spokesPerRevolution * max_spoke_len);
      return;
    }

    this.spokesPerRevolution = spokesPerRevolution;
    this.max_spoke_len = max_spoke_len;

    // CPU-side buffer for spoke data
    this.data = new Uint8Array(spokesPerRevolution * max_spoke_len);

    // Initialize polygon detection components
    this.blobDetector = new BlobDetector(
      max_spoke_len,
      spokesPerRevolution,
      this.settings.blobThreshold,
      this.settings.minBlobSize
    );
    this.contourTracer = new ContourTracer(max_spoke_len, spokesPerRevolution);
    this.hullExtractor = new ConvexHullExtractor(max_spoke_len, spokesPerRevolution);
    this.sectorExtractor = new SectorExtractor(max_spoke_len, spokesPerRevolution);

    // Create polar data texture
    this.polarTexture = this.device.createTexture({
      size: [max_spoke_len, spokesPerRevolution],
      format: "r8unorm",
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });

    // Update settings buffer with actual spoke count
    this.#updateSettingsBuffer();

    this.#createPipelineAndBindGroup();
  }

  setRange(range) {
    this.range = range;
    this.redrawCanvas();
  }

  // A new "legend" of what each byte means in terms of suggested color and meaning.
  setLegend(l) {
    console.log("WebGPU setLegend, ready:", this.ready);
    if (!this.ready) {
      this.pendingLegend = l;
      return;
    }

    const colorTableData = new Uint8Array(256 * 4);
    for (let i = 0; i < l.length; i++) {
      colorTableData[i * 4] = l[i][0];
      colorTableData[i * 4 + 1] = l[i][1];
      colorTableData[i * 4 + 2] = l[i][2];
      colorTableData[i * 4 + 3] = l[i][3];
    }

    // Create color table texture
    this.colorTexture = this.device.createTexture({
      size: [256, 1],
      format: "rgba8unorm",
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });

    this.device.queue.writeTexture(
      { texture: this.colorTexture },
      colorTableData,
      { bytesPerRow: 256 * 4 },
      { width: 256, height: 1 }
    );

    if (this.polarTexture) {
      this.#createPipelineAndBindGroup();
    }
  }

  #createPipelineAndBindGroup() {
    if (!this.polarTexture || !this.colorTexture) return;

    // Create bind group layout
    const bindGroupLayout = this.device.createBindGroupLayout({
      entries: [
        {
          binding: 0,
          visibility: GPUShaderStage.FRAGMENT,
          texture: { sampleType: "float" },
        },
        {
          binding: 1,
          visibility: GPUShaderStage.FRAGMENT,
          texture: { sampleType: "float" },
        },
        {
          binding: 2,
          visibility: GPUShaderStage.FRAGMENT,
          sampler: { type: "filtering" },
        },
        {
          binding: 3,
          visibility: GPUShaderStage.VERTEX,
          buffer: { type: "uniform" },
        },
        {
          binding: 4,
          visibility: GPUShaderStage.FRAGMENT,
          buffer: { type: "uniform" },
        },
      ],
    });

    // Create pipeline layout
    const pipelineLayout = this.device.createPipelineLayout({
      bindGroupLayouts: [bindGroupLayout],
    });

    // Create render pipeline
    this.pipeline = this.device.createRenderPipeline({
      layout: pipelineLayout,
      vertex: {
        module: this.shaderModule,
        entryPoint: "vertexMain",
        buffers: [
          {
            arrayStride: 16, // 4 floats * 4 bytes
            attributes: [
              { shaderLocation: 0, offset: 0, format: "float32x2" },  // position
              { shaderLocation: 1, offset: 8, format: "float32x2" },  // texCoord
            ],
          },
        ],
      },
      fragment: {
        module: this.shaderModule,
        entryPoint: "fragmentMain",
        targets: [{ format: this.canvasFormat }],
      },
      primitive: {
        topology: "triangle-strip",
      },
    });

    // Create bind group
    this.bindGroup = this.device.createBindGroup({
      layout: bindGroupLayout,
      entries: [
        { binding: 0, resource: this.polarTexture.createView() },
        { binding: 1, resource: this.colorTexture.createView() },
        { binding: 2, resource: this.sampler },
        { binding: 3, resource: { buffer: this.uniformBuffer } },
        { binding: 4, resource: { buffer: this.settingsBuffer } },
      ],
    });
  }

  // A new spoke has been received.
  drawSpoke(spoke) {
    if (!this.data) return;

    if (this.actual_range != spoke.range) {
      this.actual_range = spoke.range;
      this.redrawCanvas();
    }

    let offset = spoke.angle * this.max_spoke_len;
    this.data.set(spoke.data, offset);
    if (spoke.data.length < this.max_spoke_len) {
      this.data.fill(0, offset + spoke.data.length, offset + this.max_spoke_len);
    }
  }

  // Render accumulated spokes to screen
  render() {
    if (!this.ready || !this.data || !this.pipeline) {
      return;
    }

    // Upload spoke data to GPU (always needed for shader-based rendering)
    this.device.queue.writeTexture(
      { texture: this.polarTexture },
      this.data,
      { bytesPerRow: this.max_spoke_len },
      { width: this.max_spoke_len, height: this.spokesPerRevolution }
    );

    // Polygon-based rendering
    if (this.settings.usePolygons && this.blobDetector && this.polygonRenderer) {
      const now = performance.now();

      // Throttle polygon updates for performance
      if (now - this.lastPolygonUpdate > this.polygonUpdateInterval) {
        this.lastPolygonUpdate = now;
        this.#updatePolygons();
      }

      // Render polygons
      this.#renderPolygons();
      return;
    }

    // Traditional shader-based rendering
    const encoder = this.device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view: this.context.getCurrentTexture().createView(),
          clearValue: { r: 0.0, g: 0.0, b: 0.0, a: 0.0 },  // Transparent background
          loadOp: "clear",
          storeOp: "store",
        },
      ],
    });

    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, this.bindGroup);
    pass.setVertexBuffer(0, this.vertexBuffer);
    pass.draw(4);
    pass.end();

    this.device.queue.submit([encoder.finish()]);
  }

  // Process spoke data into polygons
  #updatePolygons() {
    if (!this.blobDetector || !this.sectorExtractor || !this.data) return;

    // Update blob detector settings
    this.blobDetector.setThreshold(this.settings.blobThreshold);
    this.blobDetector.setMinBlobSize(this.settings.minBlobSize);

    // Detect blobs in spoke data
    const blobs = this.blobDetector.detect(this.data);

    // Debug: log blob detection results periodically
    if (!this._lastBlobLog || performance.now() - this._lastBlobLog > 2000) {
      this._lastBlobLog = performance.now();
      // Check if there's any data above threshold
      let maxVal = 0, countAbove = 0;
      for (let i = 0; i < this.data.length; i++) {
        if (this.data[i] > maxVal) maxVal = this.data[i];
        if (this.data[i] >= this.settings.blobThreshold) countAbove++;
      }
      console.log(`Polygon debug: maxVal=${maxVal}, countAbove=${countAbove}, blobs=${blobs.length}, threshold=${this.settings.blobThreshold}`);

      // Log blob sizes
      if (blobs.length > 0) {
        const blobSizes = blobs.slice(0, 5).map(b => b.pixelCount);
        console.log(`  Top blob sizes: ${blobSizes.join(', ')}`);
      }
    }

    if (blobs.length === 0) {
      this.polygonRenderer.updatePolygons([], [], []);
      return;
    }

    // Extract filled sectors for each blob
    // This creates pie/wedge shapes that properly fill the radar returns
    const sectors = this.sectorExtractor.extractAll(blobs);

    // Debug: show sector details
    if (!this._lastSectorLog || performance.now() - this._lastSectorLog > 2000) {
      this._lastSectorLog = performance.now();
      if (sectors.length > 0) {
        const sample = sectors[0];
        console.log(`Sectors extracted: ${sectors.length} from ${blobs.length} blobs. First sector: ${sample.cartesianVertices.length} vertices`);
        if (sample.bounds) {
          const { minR, maxR, angularSpanRad, spokeSpan } = sample.bounds;
          const angularSpanDeg = (angularSpanRad || 0) * (180 / Math.PI);
          console.log(`  Sector bounds: R=[${minR.toFixed(3)}, ${maxR.toFixed(3)}], angular span: ${angularSpanDeg.toFixed(1)}° (${spokeSpan || 0} spokes)`);
        }
        if (sample.cartesianVertices.length > 0) {
          // Calculate bounding box
          let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
          for (const v of sample.cartesianVertices) {
            minX = Math.min(minX, v.x);
            maxX = Math.max(maxX, v.x);
            minY = Math.min(minY, v.y);
            maxY = Math.max(maxY, v.y);
          }
          console.log(`  Cartesian bounds: [${minX.toFixed(4)}, ${maxX.toFixed(4)}] x [${minY.toFixed(4)}, ${maxY.toFixed(4)}]`);
        }
      } else {
        console.log(`Sectors extracted: 0 from ${blobs.length} blobs`);
      }
    }

    if (sectors.length === 0) {
      this.polygonRenderer.updatePolygons([], [], []);
      return;
    }

    // Collect polygons from sectors
    const polygons = [];
    const colors = [];

    for (const sector of sectors) {
      // Skip sectors with too few vertices
      if (sector.cartesianVertices.length < 3) {
        continue;
      }

      polygons.push(sector.cartesianVertices);

      // Color based on intensity (orange to red gradient)
      const intensity = sector.blob.avgIntensity / 255;
      colors.push({
        r: 1.0,
        g: 0.5 * (1 - intensity),
        b: 0.0,
        a: 0.8 + 0.2 * intensity
      });
    }

    // Log polygon sizes
    if (!this._lastPolyLog || performance.now() - this._lastPolyLog > 2000) {
      this._lastPolyLog = performance.now();
      if (polygons.length > 0) {
        const totalVerts = polygons.reduce((sum, p) => sum + p.length, 0);
        const avgVerts = (totalVerts / polygons.length).toFixed(1);
        const maxVerts = Math.max(...polygons.map(p => p.length));
        const minVerts = Math.min(...polygons.map(p => p.length));
        console.log(`Polygons: ${polygons.length}, total vertices: ${totalVerts}, avg: ${avgVerts}, range: ${minVerts}-${maxVerts}`);
      }
    }

    // Triangulate all polygons
    if (polygons.length > 0) {
      const triangulation = Triangulator.triangulateAll(polygons, colors);
      if (!this._lastTriLog || performance.now() - this._lastTriLog > 2000) {
        this._lastTriLog = performance.now();
        console.log(`Triangulation result: ${triangulation.vertexCount} vertices, ${triangulation.triangleCount} triangles`);
      }
      this.polygonRenderer.updateFromTriangulation(triangulation);
    } else {
      this.polygonRenderer.updatePolygons([], [], []);
    }
  }

  // Render the polygons to the canvas
  #renderPolygons() {
    if (!this.polygonRenderer) return;

    // Set transformation matrix for polygon renderer
    this.polygonRenderer.setTransform(this.#getPolygonTransformMatrix());

    // Render using standalone method (creates own encoder)
    this.polygonRenderer.renderStandalone();
  }

  // Get transformation matrix for polygon rendering
  #getPolygonTransformMatrix() {
    // Polygons are in Cartesian coordinates [-1, 1] from contour tracer
    // WebGPU clip space is also [-1, 1], so we just need aspect ratio and rotation

    // Aspect ratio correction to maintain circular shape
    const aspectRatio = this.width && this.height ? this.width / this.height : 1.0;

    // Scale to fit in viewport
    let scaleX = 1.0;
    let scaleY = 1.0;

    if (aspectRatio > 1.0) {
      // Wider than tall - shrink X
      scaleX = 1.0 / aspectRatio;
    } else {
      // Taller than wide - shrink Y
      scaleY = aspectRatio;
    }

    // Rotation by 90 degrees (radar north-up)
    const angle = Math.PI / 2;
    const cos = Math.cos(angle);
    const sin = Math.sin(angle);

    // Combined rotation and scaling matrix (column-major for WebGPU)
    // Column-major means we store columns consecutively:
    // [ m00, m10, m20, m30,   // Column 0
    //   m01, m11, m21, m31,   // Column 1
    //   m02, m12, m22, m32,   // Column 2
    //   m03, m13, m23, m33 ]  // Column 3
    //
    // For 2D rotation R(θ) applied then scale S:
    // Result = S * R
    // [ sx*cos  -sy*sin   0   0 ]
    // [ sx*sin   sy*cos   0   0 ]
    // [   0        0      1   0 ]
    // [   0        0      0   1 ]
    const matrix = new Float32Array([
      scaleX * cos,   scaleX * sin, 0.0, 0.0,   // Column 0: transforms x input
     -scaleY * sin,   scaleY * cos, 0.0, 0.0,   // Column 1: transforms y input
      0.0, 0.0, 1.0, 0.0,                        // Column 2
      0.0, 0.0, 0.0, 1.0,                        // Column 3
    ]);

    // Debug: log matrix values periodically
    if (!this._lastMatrixLog || performance.now() - this._lastMatrixLog > 5000) {
      this._lastMatrixLog = performance.now();
      console.log(`Polygon transform: scaleX=${scaleX.toFixed(4)}, scaleY=${scaleY.toFixed(4)}, aspect=${aspectRatio.toFixed(2)}, width=${this.width}, height=${this.height}`);
      console.log(`  Matrix: [${matrix[0].toFixed(3)}, ${matrix[1].toFixed(3)}, ..., ${matrix[4].toFixed(3)}, ${matrix[5].toFixed(3)}, ...]`);
    }

    return matrix;
  }

  // Called on initial setup and whenever the canvas size changes.
  redrawCanvas() {
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

    this.drawBackgroundCallback(this, "MAYARA (WebGPU)");

    if (this.ready) {
      this.context.configure({
        device: this.device,
        format: this.canvasFormat,
        alphaMode: "premultiplied",
      });
      this.#setTransformationMatrix();
    }
  }

  #setTransformationMatrix() {
    const range = this.range || this.actual_range || 1500;
    const scale = (1.0 * this.actual_range) / range;
    const angle = Math.PI / 2;

    const scaleX = scale * ((2 * this.beam_length) / this.width);
    const scaleY = scale * ((2 * this.beam_length) / this.height);

    // Combined rotation and scaling matrix (column-major for WebGPU)
    const cos = Math.cos(angle);
    const sin = Math.sin(angle);

    const transformMatrix = new Float32Array([
      cos * scaleX, -sin * scaleX, 0.0, 0.0,
      sin * scaleY,  cos * scaleY, 0.0, 0.0,
      0.0, 0.0, 1.0, 0.0,
      0.0, 0.0, 0.0, 1.0,
    ]);

    this.device.queue.writeBuffer(this.uniformBuffer, 0, transformMatrix);

    this.background_ctx.fillStyle = "lightgreen";
    this.background_ctx.fillText("Beamlength " + this.beam_length, 5, 40);
    this.background_ctx.fillText(
      "Range " + formatRangeValue(is_metric(range), range),
      5,
      60
    );
    this.background_ctx.fillText("Spoke " + this.actual_range, 5, 80);
  }
}

const shaderCode = `
const PI: f32 = 3.14159265359;

struct VertexOutput {
  @builtin(position) position: vec4<f32>,
  @location(0) texCoord: vec2<f32>,
}

struct Settings {
  transparencyThreshold: f32,
  fillMultiplier: f32,
  radialFill: f32,
  sampleCount: f32,
  interpolation: f32,
  gamma: f32,
  edgeSoftness: f32,
  spokeCount: f32,
}

@group(0) @binding(3) var<uniform> u_transform: mat4x4<f32>;
@group(0) @binding(4) var<uniform> settings: Settings;

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

@group(0) @binding(0) var polarData: texture_2d<f32>;
@group(0) @binding(1) var colorTable: texture_2d<f32>;
@group(0) @binding(2) var texSampler: sampler;

@fragment
fn fragmentMain(@location(0) texCoord: vec2<f32>) -> @location(0) vec4<f32> {
  // Convert texture coordinates to polar coordinates
  let centered = texCoord - vec2<f32>(0.5, 0.5);
  let r = length(centered) * 2.0;

  // Outside radar circle -> transparent
  let insideCircle = step(r, 1.0);

  // Calculate angle, normalize to [0, 1]
  var theta = atan2(centered.y, centered.x);
  if (theta < 0.0) {
    theta = theta + 2.0 * PI;
  }
  let normalizedTheta = theta / (2.0 * PI);

  // Angular interpolation between adjacent spokes
  let spokeCount = settings.spokeCount;
  let spoke_f = normalizedTheta * spokeCount;
  let spoke0 = floor(spoke_f);
  let spoke1 = spoke0 + 1.0;
  let t = fract(spoke_f);

  // Sample two adjacent spokes for interpolation
  let uv0 = vec2<f32>(r, spoke0 / spokeCount);
  let uv1 = vec2<f32>(r, spoke1 / spokeCount);
  let idx0 = textureSample(polarData, texSampler, uv0).r;
  let idx1 = textureSample(polarData, texSampler, uv1).r;

  // Interpolated value between spokes
  let interpIndex = mix(idx0, idx1, t);

  // Angular fill expansion sampling
  // Scale angular step with radius (arc length correction: gaps grow with distance)
  let angularStep = settings.fillMultiplier / spokeCount * max(1.0, r * 2.0);
  // Radial step: scale to be meaningful relative to spoke data resolution
  // With 883 pixels over r=0..1, each pixel is ~0.00113, so 0.003 covers ~2-3 pixels
  let radialStep = settings.radialFill * 0.003;

  // Sample grid around the pixel for better fill
  // Angular samples at different offsets
  var maxIndex = interpIndex * settings.interpolation;

  // Sample 1: center
  let s0 = textureSample(polarData, texSampler, vec2<f32>(r, normalizedTheta)).r;
  maxIndex = max(maxIndex, s0);

  // Sample 2-3: angular +/-
  let s1 = textureSample(polarData, texSampler, vec2<f32>(r, normalizedTheta + angularStep)).r;
  let s2 = textureSample(polarData, texSampler, vec2<f32>(r, normalizedTheta - angularStep)).r;
  maxIndex = max(maxIndex, max(s1, s2));

  // Sample radial +/- (always active - critical for connecting patches)
  let sr1 = textureSample(polarData, texSampler, vec2<f32>(r + radialStep, normalizedTheta)).r;
  let sr2 = textureSample(polarData, texSampler, vec2<f32>(r - radialStep, normalizedTheta)).r;
  maxIndex = max(maxIndex, max(sr1, sr2));

  // Sample 4-5: angular +/- x2 (if sampleCount > 3)
  let s3 = textureSample(polarData, texSampler, vec2<f32>(r, normalizedTheta + angularStep * 2.0)).r;
  let s4 = textureSample(polarData, texSampler, vec2<f32>(r, normalizedTheta - angularStep * 2.0)).r;
  let use45 = step(3.0, settings.sampleCount);
  maxIndex = max(maxIndex, max(s3, s4) * use45);

  // Sample 6-7: angular +/- x3 (if sampleCount > 5)
  let s5 = textureSample(polarData, texSampler, vec2<f32>(r, normalizedTheta + angularStep * 3.0)).r;
  let s6 = textureSample(polarData, texSampler, vec2<f32>(r, normalizedTheta - angularStep * 3.0)).r;
  let use67 = step(5.0, settings.sampleCount);
  maxIndex = max(maxIndex, max(s5, s6) * use67);

  // Sample 8-9: radial +/- (if sampleCount > 7)
  let s7 = textureSample(polarData, texSampler, vec2<f32>(r + radialStep, normalizedTheta)).r;
  let s8 = textureSample(polarData, texSampler, vec2<f32>(r - radialStep, normalizedTheta)).r;
  let use89 = step(7.0, settings.sampleCount);
  maxIndex = max(maxIndex, max(s7, s8) * use89);

  // Sample 10-13: diagonal radial+angular (if sampleCount > 9)
  let s9 = textureSample(polarData, texSampler, vec2<f32>(r + radialStep, normalizedTheta + angularStep)).r;
  let s10 = textureSample(polarData, texSampler, vec2<f32>(r + radialStep, normalizedTheta - angularStep)).r;
  let s11 = textureSample(polarData, texSampler, vec2<f32>(r - radialStep, normalizedTheta + angularStep)).r;
  let s12 = textureSample(polarData, texSampler, vec2<f32>(r - radialStep, normalizedTheta - angularStep)).r;
  let useDiag = step(9.0, settings.sampleCount);
  maxIndex = max(maxIndex, max(max(s9, s10), max(s11, s12)) * useDiag);

  // Sample 14-15: angular +/- x4 (if sampleCount > 13)
  let s13 = textureSample(polarData, texSampler, vec2<f32>(r, normalizedTheta + angularStep * 4.0)).r;
  let s14 = textureSample(polarData, texSampler, vec2<f32>(r, normalizedTheta - angularStep * 4.0)).r;
  let use1415 = step(13.0, settings.sampleCount);
  maxIndex = max(maxIndex, max(s13, s14) * use1415);

  // Apply gamma correction
  let finalIndex = pow(maxIndex, 1.0 / settings.gamma);

  // Look up color from color table
  let color = textureSample(colorTable, texSampler, vec2<f32>(finalIndex, 0.0));

  // Soft edge anti-aliasing at radar boundary
  let edgeFade = smoothstep(1.0, 1.0 - settings.edgeSoftness, r);

  // Threshold check
  let aboveThreshold = step(settings.transparencyThreshold, finalIndex);
  let alpha = insideCircle * aboveThreshold * color.a * edgeFade;

  return vec4<f32>(color.rgb * alpha, alpha);
}
`;
