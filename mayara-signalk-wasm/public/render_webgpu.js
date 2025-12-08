export { render_webgpu };

import { RANGE_SCALE, formatRangeValue, is_metric } from "./viewer.js";

class render_webgpu {
  constructor(canvas_dom, canvas_background_dom, drawBackground) {
    this.dom = canvas_dom;
    this.background_dom = canvas_background_dom;
    this.background_ctx = this.background_dom.getContext("2d");
    this.drawBackgroundCallback = drawBackground;

    this.actual_range = 0;
    this.ready = false;
    this.pendingLegend = null;
    this.pendingSpokes = null;

    // Accumulation settings
    this.decay = 0.985;           // Temporal persistence (lower = faster fade)
    this.accumGain = 0.4;         // How much new data adds to accumulation
    this.accumulationSize = 1024; // Cartesian accumulation texture size

    // Ping-pong texture index (0 or 1)
    this.pingPongIndex = 0;

    // Start async initialization
    this.initPromise = this.#initWebGPU();
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

    // Create sampler
    this.sampler = this.device.createSampler({
      magFilter: "linear",
      minFilter: "linear",
      addressModeU: "clamp-to-edge",
      addressModeV: "clamp-to-edge",
    });

    // Create uniform buffer for transformation matrix
    this.uniformBuffer = this.device.createBuffer({
      size: 64,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });

    // Create vertex buffer for fullscreen quad
    const vertices = new Float32Array([
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

    // Create ping-pong accumulation textures (rgba8unorm supports render target)
    this.accumTextures = [
      this.device.createTexture({
        size: [this.accumulationSize, this.accumulationSize],
        format: "rgba8unorm",
        usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.RENDER_ATTACHMENT,
      }),
      this.device.createTexture({
        size: [this.accumulationSize, this.accumulationSize],
        format: "rgba8unorm",
        usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.RENDER_ATTACHMENT,
      }),
    ];

    // Create accumulation parameters buffer [decay, gain, spokesPerRev, maxSpokeLen]
    this.accumParamsBuffer = this.device.createBuffer({
      size: 16,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });

    // Create accumulation pipeline (renders polar data into cartesian accumulation texture)
    await this.#createAccumulationPipeline();

    this.ready = true;
    this.redrawCanvas();

    if (this.pendingSpokes) {
      this.setSpokes(this.pendingSpokes.spokesPerRevolution, this.pendingSpokes.max_spoke_len);
      this.pendingSpokes = null;
    }
    if (this.pendingLegend) {
      this.setLegend(this.pendingLegend);
      this.pendingLegend = null;
    }
    console.log("WebGPU initialized with ping-pong accumulation");
  }

  async #createAccumulationPipeline() {
    // Accumulation shader - combines previous accumulation with new polar data
    const accumShaderModule = this.device.createShaderModule({
      code: accumulationShaderCode,
    });

    this.accumBindGroupLayout = this.device.createBindGroupLayout({
      entries: [
        { binding: 0, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } }, // previous accum
        { binding: 1, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } }, // polar data
        { binding: 2, visibility: GPUShaderStage.FRAGMENT, sampler: { type: "filtering" } },
        { binding: 3, visibility: GPUShaderStage.FRAGMENT, buffer: { type: "uniform" } },      // params
      ],
    });

    this.accumPipeline = this.device.createRenderPipeline({
      layout: this.device.createPipelineLayout({ bindGroupLayouts: [this.accumBindGroupLayout] }),
      vertex: {
        module: accumShaderModule,
        entryPoint: "vertexMain",
        buffers: [{
          arrayStride: 16,
          attributes: [
            { shaderLocation: 0, offset: 0, format: "float32x2" },
            { shaderLocation: 1, offset: 8, format: "float32x2" },
          ],
        }],
      },
      fragment: {
        module: accumShaderModule,
        entryPoint: "fragmentMain",
        targets: [{ format: "rgba8unorm" }],
      },
      primitive: { topology: "triangle-strip" },
    });

    // Final render shader - displays accumulation with color lookup
    const renderShaderModule = this.device.createShaderModule({
      code: finalRenderShaderCode,
    });

    this.renderBindGroupLayout = this.device.createBindGroupLayout({
      entries: [
        { binding: 0, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } }, // accumulation
        { binding: 1, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } }, // color table
        { binding: 2, visibility: GPUShaderStage.FRAGMENT, sampler: { type: "filtering" } },
        { binding: 3, visibility: GPUShaderStage.VERTEX, buffer: { type: "uniform" } },
      ],
    });

    this.renderPipeline = this.device.createRenderPipeline({
      layout: this.device.createPipelineLayout({ bindGroupLayouts: [this.renderBindGroupLayout] }),
      vertex: {
        module: renderShaderModule,
        entryPoint: "vertexMain",
        buffers: [{
          arrayStride: 16,
          attributes: [
            { shaderLocation: 0, offset: 0, format: "float32x2" },
            { shaderLocation: 1, offset: 8, format: "float32x2" },
          ],
        }],
      },
      fragment: {
        module: renderShaderModule,
        entryPoint: "fragmentMain",
        targets: [{ format: this.canvasFormat }],
      },
      primitive: { topology: "triangle-strip" },
    });
  }

  setSpokes(spokesPerRevolution, max_spoke_len) {
    console.log("WebGPU setSpokes:", spokesPerRevolution, max_spoke_len, "ready:", this.ready);

    if (!this.ready) {
      this.pendingSpokes = { spokesPerRevolution, max_spoke_len };
      this.spokesPerRevolution = spokesPerRevolution;
      this.max_spoke_len = max_spoke_len;
      this.data = new Uint8Array(spokesPerRevolution * max_spoke_len);
      return;
    }

    this.spokesPerRevolution = spokesPerRevolution;
    this.max_spoke_len = max_spoke_len;
    this.data = new Uint8Array(spokesPerRevolution * max_spoke_len);

    // Create polar data texture
    this.polarTexture = this.device.createTexture({
      size: [max_spoke_len, spokesPerRevolution],
      format: "r8unorm",
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });

    // Update accumulation params
    this.device.queue.writeBuffer(
      this.accumParamsBuffer, 0,
      new Float32Array([this.decay, this.accumGain, spokesPerRevolution, max_spoke_len])
    );

    this.#createBindGroups();
  }

  setRange(range) {
    this.range = range;
    this.redrawCanvas();
  }

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
      this.#createBindGroups();
    }
  }

  #createBindGroups() {
    if (!this.polarTexture || !this.colorTexture) return;

    // Create bind groups for both ping-pong directions
    this.accumBindGroups = [
      // Read from texture 0, write to texture 1
      this.device.createBindGroup({
        layout: this.accumBindGroupLayout,
        entries: [
          { binding: 0, resource: this.accumTextures[0].createView() },
          { binding: 1, resource: this.polarTexture.createView() },
          { binding: 2, resource: this.sampler },
          { binding: 3, resource: { buffer: this.accumParamsBuffer } },
        ],
      }),
      // Read from texture 1, write to texture 0
      this.device.createBindGroup({
        layout: this.accumBindGroupLayout,
        entries: [
          { binding: 0, resource: this.accumTextures[1].createView() },
          { binding: 1, resource: this.polarTexture.createView() },
          { binding: 2, resource: this.sampler },
          { binding: 3, resource: { buffer: this.accumParamsBuffer } },
        ],
      }),
    ];

    // Create render bind groups for both textures
    this.renderBindGroups = [
      this.device.createBindGroup({
        layout: this.renderBindGroupLayout,
        entries: [
          { binding: 0, resource: this.accumTextures[1].createView() }, // After writing to 1
          { binding: 1, resource: this.colorTexture.createView() },
          { binding: 2, resource: this.sampler },
          { binding: 3, resource: { buffer: this.uniformBuffer } },
        ],
      }),
      this.device.createBindGroup({
        layout: this.renderBindGroupLayout,
        entries: [
          { binding: 0, resource: this.accumTextures[0].createView() }, // After writing to 0
          { binding: 1, resource: this.colorTexture.createView() },
          { binding: 2, resource: this.sampler },
          { binding: 3, resource: { buffer: this.uniformBuffer } },
        ],
      }),
    ];
  }

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

  render() {
    if (!this.ready || !this.data || !this.accumBindGroups || !this.renderBindGroups) {
      return;
    }

    // Upload spoke data to GPU
    this.device.queue.writeTexture(
      { texture: this.polarTexture },
      this.data,
      { bytesPerRow: this.max_spoke_len },
      { width: this.max_spoke_len, height: this.spokesPerRevolution }
    );

    const encoder = this.device.createCommandEncoder();

    // Pass 1: Accumulation - read from texture[pingPongIndex], write to texture[1-pingPongIndex]
    const writeIndex = 1 - this.pingPongIndex;
    const accumPass = encoder.beginRenderPass({
      colorAttachments: [{
        view: this.accumTextures[writeIndex].createView(),
        loadOp: "load",  // Keep previous data for decay
        storeOp: "store",
      }],
    });
    accumPass.setPipeline(this.accumPipeline);
    accumPass.setBindGroup(0, this.accumBindGroups[this.pingPongIndex]);
    accumPass.setVertexBuffer(0, this.vertexBuffer);
    accumPass.draw(4);
    accumPass.end();

    // Pass 2: Final render to screen
    const renderPass = encoder.beginRenderPass({
      colorAttachments: [{
        view: this.context.getCurrentTexture().createView(),
        clearValue: { r: 0.0, g: 0.0, b: 0.0, a: 0.0 },
        loadOp: "clear",
        storeOp: "store",
      }],
    });
    renderPass.setPipeline(this.renderPipeline);
    renderPass.setBindGroup(0, this.renderBindGroups[this.pingPongIndex]);
    renderPass.setVertexBuffer(0, this.vertexBuffer);
    renderPass.draw(4);
    renderPass.end();

    this.device.queue.submit([encoder.finish()]);

    // Swap ping-pong index
    this.pingPongIndex = writeIndex;
  }

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

    this.drawBackgroundCallback(this, "MAYARA (WebGPU Accum)");

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
    this.background_ctx.fillText("Range " + formatRangeValue(is_metric(range), range), 5, 60);
    this.background_ctx.fillText("Spoke " + this.actual_range, 5, 80);
    this.background_ctx.fillText("Decay " + this.decay.toFixed(3), 5, 100);
  }
}

// Accumulation shader - converts polar to cartesian and accumulates
const accumulationShaderCode = `
struct VertexOutput {
  @builtin(position) position: vec4<f32>,
  @location(0) texCoord: vec2<f32>,
}

@vertex
fn vertexMain(@location(0) pos: vec2<f32>, @location(1) texCoord: vec2<f32>) -> VertexOutput {
  var output: VertexOutput;
  output.position = vec4<f32>(pos, 0.0, 1.0);
  output.texCoord = texCoord;
  return output;
}

@group(0) @binding(0) var prevAccum: texture_2d<f32>;
@group(0) @binding(1) var polarData: texture_2d<f32>;
@group(0) @binding(2) var texSampler: sampler;
@group(0) @binding(3) var<uniform> params: vec4<f32>;  // [decay, gain, spokesPerRev, maxSpokeLen]

const PI: f32 = 3.14159265359;

@fragment
fn fragmentMain(@location(0) texCoord: vec2<f32>) -> @location(0) vec4<f32> {
  let decay = params.x;
  let gain = params.y;

  // Previous accumulated value (decayed)
  let prevValue = textureSample(prevAccum, texSampler, texCoord).r * decay;

  // Convert cartesian (texCoord) to polar for sampling radar data
  // Accumulation texture is square [0,1]x[0,1], center at (0.5, 0.5)
  // Final render applies 90° rotation, so pre-rotate by -90°
  let centered = texCoord - vec2<f32>(0.5, 0.5);

  // Pre-rotate to compensate for final render transformation
  let rotated = vec2<f32>(centered.y, centered.x);
  let r = length(rotated) * 2.0;
  let theta = atan2(rotated.y, rotated.x);

  // Normalize theta to [0, 1] range
  let normalizedTheta = 1.0 - (theta + PI) / (2.0 * PI);

  // Sample polar data (new radar return)
  let newValue = textureSample(polarData, texSampler, vec2<f32>(r, normalizedTheta)).r;

  // Accumulate: add new value (scaled) to decayed previous
  // Use max to prevent washing out strong returns
  var accumulated = max(prevValue, prevValue + newValue * gain);
  accumulated = clamp(accumulated, 0.0, 1.0);

  // Mask outside circle
  let insideCircle = step(r, 1.0);
  accumulated *= insideCircle;

  return vec4<f32>(accumulated, accumulated, accumulated, 1.0);
}
`;

// Final render shader - applies color lookup to accumulated data
const finalRenderShaderCode = `
struct VertexOutput {
  @builtin(position) position: vec4<f32>,
  @location(0) texCoord: vec2<f32>,
}

@group(0) @binding(3) var<uniform> u_transform: mat4x4<f32>;

@vertex
fn vertexMain(@location(0) pos: vec2<f32>, @location(1) texCoord: vec2<f32>) -> VertexOutput {
  var output: VertexOutput;
  output.position = u_transform * vec4<f32>(pos, 0.0, 1.0);
  output.texCoord = texCoord;
  return output;
}

@group(0) @binding(0) var accumTex: texture_2d<f32>;
@group(0) @binding(1) var colorTable: texture_2d<f32>;
@group(0) @binding(2) var texSampler: sampler;

@fragment
fn fragmentMain(@location(0) texCoord: vec2<f32>) -> @location(0) vec4<f32> {
  // Sample accumulated energy
  let energy = textureSample(accumTex, texSampler, texCoord).r;

  // Look up color from table
  let color = textureSample(colorTable, texSampler, vec2<f32>(energy, 0.0));

  // Threshold for visibility
  let hasData = step(0.01, energy);
  let alpha = hasData * color.a;

  return vec4<f32>(color.rgb, alpha);
}
`;
