export { WebGPURenderer };

/**
 * WebGPU Renderer - Handles only the WebGPU-specific spoke rendering
 * This is a backend renderer that can be used with the PPI class
 */
class WebGPURenderer {
  constructor(canvas_dom) {
    this.dom = canvas_dom;
    this.ready = false;
    this.pendingLegend = null;
    this.pendingSpokes = null;

    // Spoke data
    this.spokesPerRevolution = 0;
    this.maxspokelength = 0;

    // Display parameters (set by PPI via resize)
    this.width = 0;
    this.height = 0;
    this.beam_length = 0;
    this.headingRotation = 0;
    this.range = 0;
    this.actual_range = 0;

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

    // Create sampler for polar data (linear interpolation)
    this.sampler = this.device.createSampler({
      magFilter: "linear",
      minFilter: "linear",
      addressModeU: "clamp-to-edge",
      addressModeV: "repeat",
    });

    // Create sampler for color table (nearest - no interpolation for palette lookup)
    this.colorSampler = this.device.createSampler({
      magFilter: "nearest",
      minFilter: "nearest",
      addressModeU: "clamp-to-edge",
      addressModeV: "clamp-to-edge",
    });

    // Create uniform buffer for parameters
    this.uniformBuffer = this.device.createBuffer({
      size: 32,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });

    // Create vertex buffer for fullscreen quad
    const vertices = new Float32Array([
      -1.0, -1.0, 0.0, 0.0, 1.0, -1.0, 1.0, 0.0, -1.0, 1.0, 0.0, 1.0, 1.0, 1.0,
      1.0, 1.0,
    ]);

    this.vertexBuffer = this.device.createBuffer({
      size: vertices.byteLength,
      usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
    });
    this.device.queue.writeBuffer(this.vertexBuffer, 0, vertices);

    // Create render pipeline
    await this.#createRenderPipeline();

    this.ready = true;

    // Process any pending data
    if (this.pendingSpokes) {
      this.setSpokes(
        this.pendingSpokes.spokesPerRevolution,
        this.pendingSpokes.maxspokelength
      );
      this.pendingSpokes = null;
    }
    if (this.pendingLegend) {
      this.setLegend(this.pendingLegend);
      this.pendingLegend = null;
    }

    console.log("WebGPU initialized (direct polar rendering)");
  }

  async #createRenderPipeline() {
    const shaderModule = this.device.createShaderModule({
      code: shaderCode,
    });

    this.bindGroupLayout = this.device.createBindGroupLayout({
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
          visibility: GPUShaderStage.VERTEX | GPUShaderStage.FRAGMENT,
          buffer: { type: "uniform" },
        },
        {
          binding: 4,
          visibility: GPUShaderStage.FRAGMENT,
          sampler: { type: "non-filtering" },
        },
      ],
    });

    this.renderPipeline = this.device.createRenderPipeline({
      layout: this.device.createPipelineLayout({
        bindGroupLayouts: [this.bindGroupLayout],
      }),
      vertex: {
        module: shaderModule,
        entryPoint: "vertexMain",
        buffers: [
          {
            arrayStride: 16,
            attributes: [
              { shaderLocation: 0, offset: 0, format: "float32x2" },
              { shaderLocation: 1, offset: 8, format: "float32x2" },
            ],
          },
        ],
      },
      fragment: {
        module: shaderModule,
        entryPoint: "fragmentMain",
        targets: [{ format: this.canvasFormat }],
      },
      primitive: { topology: "triangle-strip" },
    });
  }

  /**
   * Initialize spoke texture dimensions
   */
  setSpokes(spokesPerRevolution, maxspokelength) {
    if (!this.ready) {
      this.pendingSpokes = { spokesPerRevolution, maxspokelength };
      this.spokesPerRevolution = spokesPerRevolution;
      this.maxspokelength = maxspokelength;
      return;
    }

    console.log(
      `WebGPU: setSpokes ${this.spokesPerRevolution}x${this.maxspokelength} -> ${spokesPerRevolution}x${maxspokelength}`
    );

    this.spokesPerRevolution = spokesPerRevolution;
    this.maxspokelength = maxspokelength;

    // Create polar data texture with new dimensions
    this.polarTexture = this.device.createTexture({
      size: [maxspokelength, spokesPerRevolution],
      format: "r8unorm",
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });

    this.#createBindGroup();
    this.#updateUniforms();
  }

  /**
   * Set the color legend/table
   * @param {object} legend - Legend with colors array
   */
  setLegend(legend) {
    if (!this.ready) {
      this.pendingLegend = legend;
      return;
    }

    const l = legend.colors;
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
      this.#createBindGroup();
    }
  }

  #createBindGroup() {
    if (!this.polarTexture || !this.colorTexture) return;

    this.bindGroup = this.device.createBindGroup({
      layout: this.bindGroupLayout,
      entries: [
        { binding: 0, resource: this.polarTexture.createView() },
        { binding: 1, resource: this.colorTexture.createView() },
        { binding: 2, resource: this.sampler },
        { binding: 3, resource: { buffer: this.uniformBuffer } },
        { binding: 4, resource: this.colorSampler },
      ],
    });
  }

  /**
   * Handle canvas resize
   * @param {number} width - Canvas width
   * @param {number} height - Canvas height
   * @param {number} beam_length - Beam length in pixels
   * @param {number} headingRotation - Heading rotation in radians
   */
  resize(width, height, beam_length, headingRotation) {
    // Only re-assign canvas dimensions and re-configure the WebGPU
    // context when the size actually changed. Re-configure is
    // expensive (drops the swap chain) and re-assigning canvas.width
    // clears the bitmap; for transform-only updates (heading rotation,
    // beam-length / zoom change) we just push the new uniforms and
    // keep the existing frame.
    const sizeChanged = this.width !== width || this.height !== height;

    this.width = width;
    this.height = height;
    this.beam_length = beam_length;
    this.headingRotation = headingRotation;

    if (sizeChanged) {
      this.dom.width = width;
      this.dom.height = height;
      if (this.ready) {
        this.context.configure({
          device: this.device,
          format: this.canvasFormat,
          alphaMode: "premultiplied",
        });
      }
    }

    if (this.ready) {
      this.#updateUniforms();
    }
  }

  /**
   * Set range for scaling
   */
  setRangeScale(range, actual_range) {
    this.range = range;
    this.actual_range = actual_range;
    if (this.ready) {
      this.#updateUniforms();
    }
  }

  /**
   * Clear the radar display
   */
  clearDisplay(data, spokesPerRevolution, maxspokelength) {
    if (this.ready && this.polarTexture && data) {
      this.device.queue.writeTexture(
        { texture: this.polarTexture },
        data,
        { bytesPerRow: maxspokelength },
        { width: maxspokelength, height: spokesPerRevolution }
      );
      this.#renderFrame();
    }
  }

  /**
   * Render the current spoke data
   * @param {Uint8Array} data - Spoke data buffer
   * @param {number} spokesPerRevolution - Spokes per revolution
   * @param {number} maxspokelength - Max spoke length
   */
  render(data, spokesPerRevolution, maxspokelength) {
    if (!this.ready || !data || !this.bindGroup) {
      return;
    }

    // Upload spoke data to GPU
    this.device.queue.writeTexture(
      { texture: this.polarTexture },
      data,
      { bytesPerRow: maxspokelength },
      { width: maxspokelength, height: spokesPerRevolution }
    );

    this.#renderFrame();
  }

  #renderFrame() {
    const encoder = this.device.createCommandEncoder();

    const renderPass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view: this.context.getCurrentTexture().createView(),
          clearValue: { r: 0.0, g: 0.0, b: 0.0, a: 0.0 },
          loadOp: "clear",
          storeOp: "store",
        },
      ],
    });
    renderPass.setPipeline(this.renderPipeline);
    renderPass.setBindGroup(0, this.bindGroup);
    renderPass.setVertexBuffer(0, this.vertexBuffer);
    renderPass.draw(4);
    renderPass.end();

    this.device.queue.submit([encoder.finish()]);
  }

  #updateUniforms() {
    const range = this.range || this.actual_range || 1;
    const actual = this.actual_range || range;
    const scale = actual / range;

    const scaleX = scale * ((2 * this.beam_length) / this.width);
    const scaleY = scale * ((2 * this.beam_length) / this.height);

    const uniforms = new Float32Array([
      scaleX,
      scaleY,
      this.spokesPerRevolution || 2048,
      this.maxspokelength || 512,
      this.headingRotation || 0,
      0,
      0,
      0,
    ]);

    this.device.queue.writeBuffer(this.uniformBuffer, 0, uniforms);
  }
}

// Direct polar-to-cartesian shader with color lookup
const shaderCode = `
struct VertexOutput {
  @builtin(position) position: vec4<f32>,
  @location(0) texCoord: vec2<f32>,
}

struct Uniforms {
  scaleX: f32,
  scaleY: f32,
  spokesPerRev: f32,
  maxSpokeLen: f32,
  headingRotation: f32,
}

@group(0) @binding(3) var<uniform> uniforms: Uniforms;

@vertex
fn vertexMain(@location(0) pos: vec2<f32>, @location(1) texCoord: vec2<f32>) -> VertexOutput {
  var output: VertexOutput;
  let scaledPos = vec2<f32>(pos.x * uniforms.scaleX, pos.y * uniforms.scaleY);
  output.position = vec4<f32>(scaledPos, 0.0, 1.0);
  output.texCoord = texCoord;
  return output;
}

@group(0) @binding(0) var polarData: texture_2d<f32>;
@group(0) @binding(1) var colorTable: texture_2d<f32>;
@group(0) @binding(2) var texSampler: sampler;
@group(0) @binding(4) var colorSampler: sampler;

const PI: f32 = 3.14159265359;
const TWO_PI: f32 = 6.28318530718;

@fragment
fn fragmentMain(@location(0) texCoord: vec2<f32>) -> @location(0) vec4<f32> {
  let centered = texCoord - vec2<f32>(0.5, 0.5);
  let r = length(centered) * 2.0;

  var theta = atan2(centered.x, centered.y);
  theta = theta - uniforms.headingRotation;

  if (theta < 0.0) {
    theta = theta + TWO_PI;
  }
  if (theta >= TWO_PI) {
    theta = theta - TWO_PI;
  }

  let normalizedTheta = theta / TWO_PI;
  let radarValue = textureSample(polarData, texSampler, vec2<f32>(r, normalizedTheta)).r;
  let color = textureSample(colorTable, colorSampler, vec2<f32>(radarValue, 0.0));

  let insideCircle = step(r, 1.0);
  let hasData = step(0.004, radarValue);
  let alpha = hasData * color.a * insideCircle;

  return vec4<f32>(color.rgb * insideCircle, alpha);
}
`;
