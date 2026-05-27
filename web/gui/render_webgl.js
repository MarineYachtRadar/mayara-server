export { WebGLRenderer };

/**
 * WebGL Renderer - Handles only the WebGL-specific spoke rendering
 * This is a backend renderer that can be used with the PPI class
 * Uses WebGL2 for polar-to-cartesian transformation
 */
class WebGLRenderer {
  constructor(canvas_dom) {
    this.dom = canvas_dom;
    this.ready = false;
    this.pendingLegend = null;
    this.pendingSpokes = null;

    // Spoke data
    this.spokesPerRevolution = 0;
    this.maxspokelength = 0;
    this.data = null;

    // Display parameters (set by PPI via resize)
    this.width = 0;
    this.height = 0;
    this.beam_length = 0;
    this.headingRotation = 0;
    this.range = 0;
    this.actual_range = 0;

    // Start initialization
    this.initPromise = this.#initWebGL();
  }

  async #initWebGL() {
    const gl = this.dom.getContext("webgl2");
    if (!gl) {
      throw new Error("WebGL2 not supported");
    }

    const vertexShader = this.#createShader(gl, gl.VERTEX_SHADER, vertexShaderSource);
    const fragmentShader = this.#createShader(gl, gl.FRAGMENT_SHADER, fragmentShaderSource);
    const program = this.#createProgram(gl, vertexShader, fragmentShader);

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

    this.transformMatrixLocation = gl.getUniformLocation(program, "u_transform");
    this.headingRotationLocation = gl.getUniformLocation(program, "u_headingRotation");

    this.gl = gl;
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

    console.log("WebGL2 initialized (polar rendering)");
  }

  #createShader(gl, type, source) {
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

  #createProgram(gl, vertexShader, fragmentShader) {
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

    this.spokesPerRevolution = spokesPerRevolution;
    this.maxspokelength = maxspokelength;
    this.data = new Uint8Array(spokesPerRevolution * maxspokelength);
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

    this.#loadColorTableTexture(colorTableData);
  }

  #loadColorTableTexture(legend) {
    const gl = this.gl;
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

    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
  }

  /**
   * Handle canvas resize
   * @param {number} width - Canvas width
   * @param {number} height - Canvas height
   * @param {number} beam_length - Beam length in pixels
   * @param {number} headingRotation - Heading rotation in radians
   */
  resize(width, height, beam_length, headingRotation) {
    // Only re-assign canvas dimensions when they actually changed.
    // Assigning to canvas.width / .height clears the bitmap and resets
    // the WebGL context's framebuffer, so for transform-only updates
    // (heading rotation, beam-length / zoom change) we skip the reset
    // and just push the new transform.
    const sizeChanged = this.width !== width || this.height !== height;

    this.width = width;
    this.height = height;
    this.beam_length = beam_length;
    this.headingRotation = headingRotation;

    if (sizeChanged) {
      this.dom.width = width;
      this.dom.height = height;
      if (this.ready) {
        this.gl.viewport(0, 0, width, height);
      }
    }

    if (this.ready) {
      this.#updateTransform();
    }
  }

  /**
   * Set range for scaling
   */
  setRangeScale(range, actual_range) {
    this.range = range;
    this.actual_range = actual_range;
    if (this.ready) {
      this.#updateTransform();
    }
  }

  #updateTransform() {
    const range = this.range || this.actual_range || 1;
    const actual = this.actual_range || range;
    const scale = actual / range;

    const scaleX = scale * ((2 * this.beam_length) / this.width);
    const scaleY = scale * ((2 * this.beam_length) / this.height);

    const transformMatrix = new Float32Array([
      scaleX, 0.0, 0.0, 0.0,
      0.0, scaleY, 0.0, 0.0,
      0.0, 0.0, 1.0, 0.0,
      0.0, 0.0, 0.0, 1.0,
    ]);

    this.gl.uniformMatrix4fv(this.transformMatrixLocation, false, transformMatrix);
    this.gl.uniform1f(this.headingRotationLocation, this.headingRotation || 0);
    this.gl.clear(this.gl.COLOR_BUFFER_BIT);
  }

  /**
   * Clear the radar display
   */
  clearDisplay(data, spokesPerRevolution, maxspokelength) {
    if (this.ready && data) {
      this.data = data;
      this.#updateTexture();
      this.#draw();
    }
  }

  /**
   * Render the current spoke data
   * @param {Uint8Array} data - Spoke data buffer
   * @param {number} spokesPerRevolution - Spokes per revolution
   * @param {number} maxspokelength - Max spoke length
   */
  render(data, spokesPerRevolution, maxspokelength) {
    if (!this.ready || !data) {
      return;
    }

    this.data = data;
    this.#updateTexture();
    this.#draw();
  }

  #updateTexture() {
    const gl = this.gl;
    gl.activeTexture(gl.TEXTURE0);
    gl.texImage2D(
      gl.TEXTURE_2D,
      0,
      gl.R8,
      this.maxspokelength,
      this.spokesPerRevolution,
      0,
      gl.RED,
      gl.UNSIGNED_BYTE,
      this.data
    );
    gl.generateMipmap(gl.TEXTURE_2D);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  }

  #draw() {
    const gl = this.gl;
    gl.clearColor(0.0, 0.0, 0.0, 0.0);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.viewport(0, 0, gl.canvas.width, gl.canvas.height);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
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

  uniform sampler2D u_polarIndexData;
  uniform sampler2D u_colorTable;
  uniform float u_headingRotation;

  const float PI = 3.14159265359;
  const float TWO_PI = 6.28318530718;

  void main() {
    vec2 centeredCoords = v_texCoord - vec2(0.5, 0.5);
    float r = length(centeredCoords) * 2.0;

    // Calculate angle with heading rotation support
    float theta = atan(centeredCoords.x, centeredCoords.y);
    theta = theta - u_headingRotation;

    if (theta < 0.0) {
      theta = theta + TWO_PI;
    }
    if (theta >= TWO_PI) {
      theta = theta - TWO_PI;
    }

    float normalizedTheta = theta / TWO_PI;
    float index = texture(u_polarIndexData, vec2(r, normalizedTheta)).r;
    vec4 lookupColor = texture(u_colorTable, vec2(index, 0.0));

    float insideCircle = step(r, 1.0);
    float hasData = step(0.004, index);
    float alpha = hasData * lookupColor.a * insideCircle;

    color = vec4(lookupColor.rgb * insideCircle, alpha);
  }
`;
