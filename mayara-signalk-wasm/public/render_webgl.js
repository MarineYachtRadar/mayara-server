export { render_webgl };

import { RANGE_SCALE, formatRangeValue, is_metric } from "./viewer.js";

class render_webgl {
  // The constructor gets two canvases, the real drawing one and one for background data
  // such as range circles etc.
  constructor(canvas_dom, canvas_background_dom, drawBackground) {
    this.dom = canvas_dom;
    this.background_dom = canvas_background_dom;
    this.background_ctx = this.background_dom.getContext("2d");
    this.drawBackgroundCallback = drawBackground;

    const gl = this.dom.getContext("webgl2");
    if (!gl) {
      throw new Error("WebGL2 not supported");
    }

    const vertexShader = createShader(gl, gl.VERTEX_SHADER, vertexShaderSource);
    const fragmentShader = createShader(
      gl,
      gl.FRAGMENT_SHADER,
      fragmentShaderSource
    );
    const program = createProgram(gl, vertexShader, fragmentShader);

    const positionBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, positionBuffer);
    const positions = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(positions), gl.STATIC_DRAW);

    const texCoordBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, texCoordBuffer);
    const texCoords = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(texCoords), gl.STATIC_DRAW);

    gl.viewport(0, 0, gl.canvas.width, gl.canvas.height);
    gl.clear(gl.COLOR_BUFFER_BIT);

    gl.useProgram(program);

    const positionLocation = gl.getAttribLocation(program, "a_position");
    gl.enableVertexAttribArray(positionLocation);
    gl.bindBuffer(gl.ARRAY_BUFFER, positionBuffer);
    gl.vertexAttribPointer(positionLocation, 2, gl.FLOAT, false, 0, 0);

    const texCoordLocation = gl.getAttribLocation(program, "a_texCoord");
    gl.enableVertexAttribArray(texCoordLocation);
    gl.bindBuffer(gl.ARRAY_BUFFER, texCoordBuffer);
    gl.vertexAttribPointer(texCoordLocation, 2, gl.FLOAT, false, 0, 0);

    gl.activeTexture(gl.TEXTURE0);
    const texture = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, texture);
    const samplerLocation = gl.getUniformLocation(program, "u_polarIndexData");
    gl.uniform1i(samplerLocation, 0);

    gl.activeTexture(gl.TEXTURE1);
    const colorLocation = gl.getUniformLocation(program, "u_colorTable");
    gl.uniform1i(colorLocation, 1);

    this.transform_matrix_location = gl.getUniformLocation(
      program,
      "u_transform"
    );

    this.gl = gl;

    this.actual_range = 0;
  }

  // This is called as soon as it is clear what the number of spokes and their max length is
  // Some brand vary the spoke length with data or range, but a promise is made about the
  // max length.
  setSpokes(spokesPerRevolution, max_spoke_len) {
    this.spokesPerRevolution = spokesPerRevolution;
    this.max_spoke_len = max_spoke_len;
    this.data = loadTexture(this.gl, spokesPerRevolution, max_spoke_len);
    console.log(`render_webgl: setSpokes(${spokesPerRevolution}, ${max_spoke_len}) - buffer size: ${this.data.length}`);
  }

  setRange(range) {
    this.range = range;
    this.redrawCanvas();
  }

  // A new "legend" of what each byte means in terms of suggested color and meaning.
  // The index is the byte value in the spoke.
  // Each entry contains a four byte array of colors and alpha (x,y,z,a).
  setLegend(l) {
    const colorTableData = new Uint8Array(256 * 4); // RGBA for each index

    for (let i = 0; i < l.length; i++) {
      colorTableData[i * 4] = l[i][0]; // Red channel
      colorTableData[i * 4 + 1] = l[i][1];
      colorTableData[i * 4 + 2] = l[i][2]; // Blue channel
      colorTableData[i * 4 + 3] = l[i][3];
    }

    loadColorTableTexture(this.gl, colorTableData);
  }

  // A new spoke has been received.
  // The spoke object contains:
  // - angle: the angle [0, spokesPerRevolution> relative to the front of the boat, clockwise.
  // - bearing: optional angle [0, spokesPerRevolution> relative to true north.
  // - range: actual range for furthest pixel, this can be (very) different from the
  //          official range passed via range().
  // - data: spoke data from closest to furthest from radome. Each byte value can be
  //         looked up in the legend.
  drawSpoke(spoke) {
    if (!this.data) {
      return;
    }
    if (this.actual_range != spoke.range) {
      this.actual_range = spoke.range;
      this.redrawCanvas();
    }

    // Bounds check to prevent buffer overflow
    if (spoke.angle >= this.spokesPerRevolution) {
      console.warn(`Spoke angle ${spoke.angle} >= spokesPerRevolution ${this.spokesPerRevolution}`);
      return;
    }

    let offset = spoke.angle * this.max_spoke_len;
    let dataLen = Math.min(spoke.data.length, this.max_spoke_len);

    // Additional bounds check
    if (offset + dataLen > this.data.length) {
      console.warn(`Buffer overflow: offset=${offset}, dataLen=${dataLen}, bufferSize=${this.data.length}`);
      return;
    }

    this.data.set(spoke.data.subarray(0, dataLen), offset);
    if (dataLen < this.max_spoke_len) {
      this.data.fill(
        0,
        offset + dataLen,
        offset + this.max_spoke_len
      );
    }
  }

  // A number of spokes has been received and now is a good time to render
  // them to the screen. Usually every 14-32 spokes.
  render() {
    if (!this.data || !this.spokesPerRevolution) {
      return;
    }
    let gl = this.gl;
    updateTexture(gl, this.data, this.spokesPerRevolution, this.max_spoke_len);
    draw(gl);
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
    this.drawBackgroundCallback(this, "MAYARA (WebGL)");
    this.#setTransformationMatrix();
  }

  #setTransformationMatrix() {
    // Use actual_range as fallback if range not set from controls
    const range = this.range || this.actual_range || 1500;
    const scale = (1.0 * this.actual_range) / range;
    // Define your rotation angle in radians (90 degrees)
    const angle = Math.PI / 2;

    const scaling_matrix = new Float32Array([
      scale * ((2 * this.beam_length) / this.width),
      0.0,
      0.0,
      0.0,
      0.0,
      scale * ((2 * this.beam_length) / this.height),
      0.0,
      0.0,
      0.0,
      0.0,
      1.0,
      0.0,
      0.0,
      0.0,
      0.0,
      1.0,
    ]);

    // Create a rotation matrix around the z-axis
    const rotation_matrix = new Float32Array([
      Math.cos(angle),
      -Math.sin(angle),
      0.0,
      0.0,
      Math.sin(angle),
      Math.cos(angle),
      0.0,
      0.0,
      0.0,
      0.0,
      1.0,
      0.0,
      0.0,
      0.0,
      0.0,
      1.0,
    ]);

    // Multiply the rotation matrix by the scaling matrix
    let transformation_matrix = new Float32Array(16);
    multiply(transformation_matrix, scaling_matrix, rotation_matrix);

    this.gl.uniformMatrix4fv(
      this.transform_matrix_location,
      false,
      transformation_matrix
    );

    this.gl.clear(this.gl.COLOR_BUFFER_BIT);

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

const vertexShaderSource = `#version 300 es
  in vec4 a_position;
  in vec2 a_texCoord;
  out vec2 v_texCoord;

  uniform mat4 u_transform;

  void main() {
    gl_Position = u_transform * a_position;
    v_texCoord = a_texCoord;
  }
`;

const fragmentShaderSource = `#version 300 es
  precision mediump float;

  in vec2 v_texCoord;
  out vec4 color;

  uniform sampler2D u_polarIndexData; // Polar data texture (contains indices)
  uniform sampler2D u_colorTable;     // Color table texture (contains RGBA values)

  void main() {
    // Convert texture coordinates into polar coordinates
    vec2 centeredCoords = v_texCoord - vec2(0.5, 0.5); // Center the coords at (0.5, 0.5)
    float r = length(centeredCoords) * 2.0; // Compute the radius
    float theta = atan(centeredCoords.y, centeredCoords.x); // Compute the angle (theta)

    // Normalize theta to be in the range [0, 1] for texture sampling
    float normalizedTheta = 1.0 - (theta + 3.14159265) / (2.0 * 3.14159265); // Map [-π, π] to [0, 1]

    // Sample directly - NEAREST filtering on the texture will snap to correct spoke
    // The texture stores spokes as rows, so theta maps to Y coordinate
    float index = texture(u_polarIndexData, vec2(r, normalizedTheta)).r;

    // Use the index to look up the color in the color table (1D texture)
    color = texture(u_colorTable, vec2(index, 0.0));
  }
`;

function createShader(gl, type, source) {
  const shader = gl.createShader(type);
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    console.error("Shader compile failed: ", gl.getShaderInfoLog(shader));
    gl.deleteShader(shader);
    return null;
  }
  return shader;
}

function createProgram(gl, vertexShader, fragmentShader) {
  const program = gl.createProgram();
  gl.attachShader(program, vertexShader);
  gl.attachShader(program, fragmentShader);
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    console.error("Program link failed: ", gl.getProgramInfoLog(program));
    gl.deleteProgram(program);
    return null;
  }
  return program;
}

function draw(gl) {
  // Clear the canvas
  gl.clearColor(0.1, 0.3, 0.1, 1.0);
  gl.clear(gl.COLOR_BUFFER_BIT);

  // Set the view port
  gl.viewport(0, 0, gl.canvas.width, gl.canvas.height);

  // Draw the square via two triangles. This is morphed to polar data
  // by the fragment shader.
  gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
}

function loadTexture(gl, spokesPerRevolution, max_spoke_len) {
  let data = new Uint8Array(spokesPerRevolution * max_spoke_len);

  return data;
}

function updateTexture(gl, data, spokesPerRevolution, max_spoke_len) {
  gl.activeTexture(gl.TEXTURE0);
  // Set UNPACK_ALIGNMENT to 1 because row width (max_spoke_len=883) is not divisible by 4
  gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
  gl.texImage2D(
    gl.TEXTURE_2D,
    0,
    gl.R8,
    max_spoke_len,
    spokesPerRevolution,
    0,
    gl.RED,
    gl.UNSIGNED_BYTE,
    data
  );
  // Use LINEAR filtering like original - interpolates between spokes for smooth appearance
  gl.generateMipmap(gl.TEXTURE_2D);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
}

function loadColorTableTexture(gl, legend) {
  // Create texture for color table
  gl.activeTexture(gl.TEXTURE1);
  const texture = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texImage2D(
    gl.TEXTURE_2D,
    0,
    gl.RGBA8,
    legend.length / 4,
    1,
    0,
    gl.RGBA,
    gl.UNSIGNED_BYTE,
    legend
  );

  // Set texture parameters - use LINEAR like original for smooth color transitions
  gl.generateMipmap(gl.TEXTURE_2D);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);

  return texture;
}

function multiply(out, a, b) {
  let a00 = a[0],
    a01 = a[1],
    a02 = a[2],
    a03 = a[3];
  let a10 = a[4],
    a11 = a[5],
    a12 = a[6],
    a13 = a[7];
  let a20 = a[8],
    a21 = a[9],
    a22 = a[10],
    a23 = a[11];
  let a30 = a[12],
    a31 = a[13],
    a32 = a[14],
    a33 = a[15];

  // Cache only the current line of the second matrix
  let b0 = b[0],
    b1 = b[1],
    b2 = b[2],
    b3 = b[3];
  out[0] = b0 * a00 + b1 * a10 + b2 * a20 + b3 * a30;
  out[1] = b0 * a01 + b1 * a11 + b2 * a21 + b3 * a31;
  out[2] = b0 * a02 + b1 * a12 + b2 * a22 + b3 * a32;
  out[3] = b0 * a03 + b1 * a13 + b2 * a23 + b3 * a33;

  b0 = b[4];
  b1 = b[5];
  b2 = b[6];
  b3 = b[7];
  out[4] = b0 * a00 + b1 * a10 + b2 * a20 + b3 * a30;
  out[5] = b0 * a01 + b1 * a11 + b2 * a21 + b3 * a31;
  out[6] = b0 * a02 + b1 * a12 + b2 * a22 + b3 * a32;
  out[7] = b0 * a03 + b1 * a13 + b2 * a23 + b3 * a33;

  b0 = b[8];
  b1 = b[9];
  b2 = b[10];
  b3 = b[11];
  out[8] = b0 * a00 + b1 * a10 + b2 * a20 + b3 * a30;
  out[9] = b0 * a01 + b1 * a11 + b2 * a21 + b3 * a31;
  out[10] = b0 * a02 + b1 * a12 + b2 * a22 + b3 * a32;
  out[11] = b0 * a03 + b1 * a13 + b2 * a23 + b3 * a33;

  b0 = b[12];
  b1 = b[13];
  b2 = b[14];
  b3 = b[15];
  out[12] = b0 * a00 + b1 * a10 + b2 * a20 + b3 * a30;
  out[13] = b0 * a01 + b1 * a11 + b2 * a21 + b3 * a31;
  out[14] = b0 * a02 + b1 * a12 + b2 * a22 + b3 * a32;
  out[15] = b0 * a03 + b1 * a13 + b2 * a23 + b3 * a33;
  return out;
}
