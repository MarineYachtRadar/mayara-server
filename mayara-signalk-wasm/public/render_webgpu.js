export { render_webgpu };

import { RANGE_SCALE, formatRangeValue, is_metric } from "./viewer.js";

class render_webgpu {
  constructor(canvas_dom, canvas_background_dom, drawBackground) {
    this.dom = canvas_dom;
    this.background_dom = canvas_background_dom;
    this.background_ctx = this.background_dom.getContext("2d");
    // Overlay canvas for range rings (on top of radar)
    this.overlay_dom = document.getElementById("myr_canvas_overlay");
    this.overlay_ctx = this.overlay_dom ? this.overlay_dom.getContext("2d") : null;
    this.drawBackgroundCallback = drawBackground;

    this.actual_range = 0;
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

    // Create sampler for polar data (linear for smooth display like TZ Pro)
    this.sampler = this.device.createSampler({
      magFilter: "linear",
      minFilter: "linear",
      addressModeU: "clamp-to-edge",
      addressModeV: "repeat",  // Wrap around for angles
    });

    // Create uniform buffer for parameters
    this.uniformBuffer = this.device.createBuffer({
      size: 32,  // scaleX, scaleY, spokesPerRev, maxSpokeLen + padding
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

    // Create render pipeline
    await this.#createRenderPipeline();

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
    console.log("WebGPU initialized (direct polar rendering)");
  }

  async #createRenderPipeline() {
    const shaderModule = this.device.createShaderModule({
      code: shaderCode,
    });

    this.bindGroupLayout = this.device.createBindGroupLayout({
      entries: [
        { binding: 0, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } }, // polar data
        { binding: 1, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } }, // color table
        { binding: 2, visibility: GPUShaderStage.FRAGMENT, sampler: { type: "filtering" } },
        { binding: 3, visibility: GPUShaderStage.VERTEX | GPUShaderStage.FRAGMENT, buffer: { type: "uniform" } },
      ],
    });

    this.renderPipeline = this.device.createRenderPipeline({
      layout: this.device.createPipelineLayout({ bindGroupLayouts: [this.bindGroupLayout] }),
      vertex: {
        module: shaderModule,
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
        module: shaderModule,
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

    // Create polar data texture (width = range samples, height = angles)
    this.polarTexture = this.device.createTexture({
      size: [max_spoke_len, spokesPerRevolution],
      format: "r8unorm",
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });

    this.#createBindGroup();
  }

  setRange(range) {
    this.range = range;
    // Clear spoke data when range changes - old data is no longer valid
    if (this.data) {
      this.data.fill(0);
    }
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
      ],
    });
  }

  drawSpoke(spoke) {
    if (!this.data) return;

    if (this.actual_range != spoke.range) {
      this.actual_range = spoke.range;
      // Clear spoke data when range changes - old data is at wrong scale
      this.data.fill(0);
      this.redrawCanvas();
    }

    // Bounds check - log bad angles
    if (spoke.angle >= this.spokesPerRevolution) {
      console.error(`Bad spoke angle: ${spoke.angle} >= ${this.spokesPerRevolution}`);
      return;
    }

    let offset = spoke.angle * this.max_spoke_len;

    // Check if data fits in buffer
    if (offset + spoke.data.length > this.data.length) {
      console.error(`Buffer overflow: offset=${offset}, data.len=${spoke.data.length}, buf.len=${this.data.length}, angle=${spoke.angle}, maxSpokeLen=${this.max_spoke_len}, spokes=${this.spokesPerRevolution}`);
      return;
    }

    // Write spoke data with neighbor enhancement
    // For each pixel, if it has a value, also boost adjacent spokes
    const spokeLen = spoke.data.length;
    const maxLen = this.max_spoke_len;
    const spokes = this.spokesPerRevolution;

    // Calculate neighbor spoke offsets (with wrap-around) - 4 spokes each direction
    const neighborOffsets = [];
    const blendFactors = [0.9, 0.75, 0.55, 0.35]; // Falloff for each distance

    for (let d = 1; d <= 4; d++) {
      const prev = (spoke.angle + spokes - d) % spokes;
      const next = (spoke.angle + d) % spokes;
      neighborOffsets.push({
        prevOffset: prev * maxLen,
        nextOffset: next * maxLen,
        blend: blendFactors[d - 1]
      });
    }

    for (let i = 0; i < spokeLen; i++) {
      const val = spoke.data[i];
      // Write current spoke at full value
      this.data[offset + i] = val;

      // Enhance neighbors if this pixel has signal
      if (val > 1) {
        for (const n of neighborOffsets) {
          const blendVal = Math.floor(val * n.blend);
          if (this.data[n.prevOffset + i] < blendVal) {
            this.data[n.prevOffset + i] = blendVal;
          }
          if (this.data[n.nextOffset + i] < blendVal) {
            this.data[n.nextOffset + i] = blendVal;
          }
        }
      }
    }

    // Clear remainder of spoke if data is shorter than max
    if (spokeLen < maxLen) {
      this.data.fill(0, offset + spokeLen, offset + maxLen);
    }
  }

  render() {
    if (!this.ready || !this.data || !this.bindGroup) {
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

    const renderPass = encoder.beginRenderPass({
      colorAttachments: [{
        view: this.context.getCurrentTexture().createView(),
        clearValue: { r: 0.0, g: 0.0, b: 0.0, a: 0.0 },
        loadOp: "clear",
        storeOp: "store",
      }],
    });
    renderPass.setPipeline(this.renderPipeline);
    renderPass.setBindGroup(0, this.bindGroup);
    renderPass.setVertexBuffer(0, this.vertexBuffer);
    renderPass.draw(4);
    renderPass.end();

    this.device.queue.submit([encoder.finish()]);
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
    if (this.overlay_dom) {
      this.overlay_dom.width = w;
      this.overlay_dom.height = h;
    }

    this.width = this.dom.width;
    this.height = this.dom.height;
    this.center_x = this.width / 2;
    this.center_y = this.height / 2;
    this.beam_length = Math.trunc(
      Math.max(this.center_x, this.center_y) * RANGE_SCALE
    );

    this.drawBackgroundCallback(this, "MAYARA (WebGPU)");
    this.#drawOverlay();

    if (this.ready) {
      this.context.configure({
        device: this.device,
        format: this.canvasFormat,
        alphaMode: "premultiplied",
      });
      this.#updateUniforms();
    }
  }

  #drawOverlay() {
    if (!this.overlay_ctx) return;

    const ctx = this.overlay_ctx;
    const range = this.range || this.actual_range;

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, this.width, this.height);

    // Draw range rings in bright green on top of radar
    ctx.strokeStyle = "#00ff00";
    ctx.lineWidth = 1.5;
    ctx.fillStyle = "#00ff00";
    ctx.font = "bold 14px/1 Verdana, Geneva, sans-serif";

    for (let i = 1; i <= 4; i++) {
      const radius = (i * this.beam_length) / 4;
      ctx.beginPath();
      ctx.arc(this.center_x, this.center_y, radius, 0, 2 * Math.PI);
      ctx.stroke();

      // Draw range labels
      if (range) {
        const text = formatRangeValue(is_metric(range), (range * i) / 4);
        // Position labels at 45 degrees (upper right)
        const labelX = this.center_x + (radius * 0.707);
        const labelY = this.center_y - (radius * 0.707);
        ctx.fillText(text, labelX + 5, labelY - 5);
      }
    }
  }

  #updateUniforms() {
    const range = this.range || this.actual_range || 1500;
    const scale = (1.0 * this.actual_range) / range;

    const scaleX = scale * ((2 * this.beam_length) / this.width);
    const scaleY = scale * ((2 * this.beam_length) / this.height);

    // Pack uniforms: scaleX, scaleY, spokesPerRev, maxSpokeLen
    const uniforms = new Float32Array([
      scaleX, scaleY,
      this.spokesPerRevolution || 2048,
      this.max_spoke_len || 512,
      0, 0, 0, 0  // padding to 32 bytes
    ]);

    this.device.queue.writeBuffer(this.uniformBuffer, 0, uniforms);

    this.background_ctx.fillStyle = "lightgreen";
    this.background_ctx.fillText("Beam length: " + this.beam_length + " px", 5, 40);
    this.background_ctx.fillText("Display range: " + formatRangeValue(is_metric(range), range), 5, 60);
    this.background_ctx.fillText("Radar range: " + formatRangeValue(is_metric(this.actual_range), this.actual_range), 5, 80);
    this.background_ctx.fillText("Spoke length: " + (this.max_spoke_len || 0) + " px", 5, 100);
  }
}

// Direct polar-to-cartesian shader with color lookup
// Radar convention: angle 0 = bow (up), angles increase CLOCKWISE
// So angle spokesPerRev/4 = starboard (right), spokesPerRev/2 = stern (down)
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
}

@group(0) @binding(3) var<uniform> uniforms: Uniforms;

@vertex
fn vertexMain(@location(0) pos: vec2<f32>, @location(1) texCoord: vec2<f32>) -> VertexOutput {
  var output: VertexOutput;
  // Apply scaling
  let scaledPos = vec2<f32>(pos.x * uniforms.scaleX, pos.y * uniforms.scaleY);
  output.position = vec4<f32>(scaledPos, 0.0, 1.0);
  output.texCoord = texCoord;
  return output;
}

@group(0) @binding(0) var polarData: texture_2d<f32>;
@group(0) @binding(1) var colorTable: texture_2d<f32>;
@group(0) @binding(2) var texSampler: sampler;

const PI: f32 = 3.14159265359;
const TWO_PI: f32 = 6.28318530718;

@fragment
fn fragmentMain(@location(0) texCoord: vec2<f32>) -> @location(0) vec4<f32> {
  // Convert cartesian (texCoord) to polar for sampling radar data
  // texCoord is [0,1]x[0,1], center at (0.5, 0.5)
  //
  // IMPORTANT: In our vertex setup, texCoord.y=0 is BOTTOM, texCoord.y=1 is TOP
  // (WebGPU clip space has Y pointing up)
  // So centered.y is POSITIVE at TOP of screen, NEGATIVE at BOTTOM
  let centered = texCoord - vec2<f32>(0.5, 0.5);

  // Calculate radius (0 at center, 1 at edge of unit circle)
  let r = length(centered) * 2.0;

  // Calculate angle from center for clockwise rotation from top (bow)
  //
  // Our coordinate system (after centering):
  // - Top of screen (bow):      centered = (0, +0.5)
  // - Right of screen (stbd):   centered = (+0.5, 0)
  // - Bottom of screen (stern): centered = (0, -0.5)
  // - Left of screen (port):    centered = (-0.5, 0)
  //
  // Radar convention (from protobuf):
  // - angle 0 = bow (top on screen)
  // - angle increases clockwise: bow -> starboard -> stern -> port -> bow
  //
  // Use atan2(x, y) to get clockwise angle from top:
  // - Top:    (0, 0.5)   -> atan2(0, 0.5) = 0
  // - Right:  (0.5, 0)   -> atan2(0.5, 0) = PI/2
  // - Bottom: (0, -0.5)  -> atan2(0, -0.5) = PI
  // - Left:   (-0.5, 0)  -> atan2(-0.5, 0) = -PI/2 -> normalized to 3PI/2
  var theta = atan2(centered.x, centered.y);
  if (theta < 0.0) {
    theta = theta + TWO_PI;
  }

  // Normalize to [0, 1] for texture V coordinate
  let normalizedTheta = theta / TWO_PI;

  // Sample polar data (always sample, mask later to avoid non-uniform control flow)
  // U = radius [0,1], V = angle [0,1] where 0=bow, 0.25=starboard, 0.5=stern, 0.75=port
  let radarValue = textureSample(polarData, texSampler, vec2<f32>(r, normalizedTheta)).r;

  // Look up color from table
  let color = textureSample(colorTable, texSampler, vec2<f32>(radarValue, 0.0));

  // Mask pixels outside the radar circle (use step instead of if)
  let insideCircle = step(r, 1.0);

  // Use alpha from color table, but make background transparent
  let hasData = step(0.004, radarValue);  // ~1/255 threshold
  let alpha = hasData * color.a * insideCircle;

  return vec4<f32>(color.rgb * insideCircle, alpha);
}
`;
