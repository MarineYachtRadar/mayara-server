"use strict";

export { RANGE_SCALE, formatRangeValue, is_metric };

import {
  loadRadar,
  registerRadarCallback,
  registerControlCallback,
  setCurrentRange,
} from "./control.js";
import "./protobuf/protobuf.min.js";

import { render_webgpu } from "./render_webgpu.js";

var webSocket;
var RadarMessage;
var renderer;
var noTransmitAngles = Array();

function divides_near(a, b) {
  let remainder = a % b;
  let r = remainder <= 1.0 || remainder >= b - 1;
  return r;
}

function is_metric(v) {
  if (v <= 100) {
    return divides_near(v, 25);
  } else if (v <= 750) {
    return divides_near(v, 50);
  }
  return divides_near(v, 500);
}

const NAUTICAL_MILE = 1852.0;

function formatRangeValue(metric, v) {
  if (metric) {
    // Metric
    v = Math.round(v);
    if (v >= 1000) {
      return v / 1000 + " km";
    } else {
      return v + " m";
    }
  } else {
    if (v >= NAUTICAL_MILE - 1) {
      if (divides_near(v, NAUTICAL_MILE)) {
        return Math.floor((v + 1) / NAUTICAL_MILE) + " nm";
      } else {
        return v / NAUTICAL_MILE + " nm";
      }
    } else if (divides_near(v, NAUTICAL_MILE / 2)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 2)) + "/2 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 4)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 4)) + "/4 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 8)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 8)) + "/8 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 16)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 16)) + "/16 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 32)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 32)) + "/32 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 64)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 64)) + "/64 nm";
    } else if (divides_near(v, NAUTICAL_MILE / 128)) {
      return Math.floor((v + 1) / (NAUTICAL_MILE / 128)) + "/128 nm";
    } else {
      return v / NAUTICAL_MILE + " nm";
    }
  }
}

const RANGE_SCALE = 0.9; // Factor by which we fill the (w,h) canvas with the outer radar range ring

registerRadarCallback(radarLoaded);
registerControlCallback(controlUpdate);

window.onload = async function () {
  const urlParams = new URLSearchParams(window.location.search);
  const id = urlParams.get("id");

  // Check WebGPU availability
  const webgpuAvailable = await checkWebGPU();
  if (!webgpuAvailable) {
    return; // Error message already shown
  }

  protobuf.load("./proto/RadarMessage.proto", function (err, root) {
    if (err) throw err;
    RadarMessage = root.lookupType(".RadarMessage");
  });

  // WebGPU only
  renderer = new render_webgpu(
    document.getElementById("myr_canvas_webgl"),
    document.getElementById("myr_canvas_background"),
    drawBackground
  );

  loadRadar(id);

  window.onresize = function () {
    renderer.redrawCanvas();
  };
};

// Check WebGPU and show error if not available
async function checkWebGPU() {
  const hasWebGPUApi = !!navigator.gpu;
  const isSecure = window.isSecureContext;

  if (!hasWebGPUApi) {
    showWebGPUError("no-api", hasWebGPUApi, isSecure);
    return false;
  }

  try {
    const adapter = await navigator.gpu.requestAdapter();
    if (!adapter) {
      showWebGPUError("no-adapter", hasWebGPUApi, isSecure);
      return false;
    }
    return true;
  } catch (e) {
    showWebGPUError("adapter-error", hasWebGPUApi, isSecure);
    return false;
  }
}

function showWebGPUError(failureReason, hasWebGPUApi, isSecure) {
  const container = document.querySelector('.myr_container');
  if (!container) return;

  const os = detectOS();
  const browser = detectBrowser();
  const hostname = window.location.hostname;
  const port = window.location.port || '80';

  // Build error message based on failure reason
  let errorMessage = '';
  if (failureReason === 'no-api' && !isSecure) {
    errorMessage = 'WebGPU API not available - likely due to insecure context.';
  } else if (failureReason === 'no-api') {
    errorMessage = 'WebGPU API not available in this browser.';
  } else if (failureReason === 'no-adapter') {
    errorMessage = 'No WebGPU adapter found. Your GPU may not support WebGPU.';
  } else {
    errorMessage = 'WebGPU initialization failed.';
  }

  container.innerHTML = `
    <div class="myr_webgpu_error">
      <h2>WebGPU Required</h2>
      <p class="myr_error_message">${errorMessage}</p>

      ${!isSecure ? `
        <div class="myr_error_section">
          <h3>Secure Context Required</h3>
          <p>WebGPU requires a secure context. You are accessing via HTTP on "${hostname}".</p>
          ${getSecureContextOptionsHTML(browser, os, port)}
        </div>
      ` : ''}

      <div class="myr_error_section">
        <h3>Enable WebGPU / Hardware Acceleration</h3>
        ${getBrowserInstructionsHTML(browser, os)}
      </div>

      <div class="myr_error_actions">
        <a href="index.html" class="myr_back_link">Back to Radar List</a>
        <button onclick="location.reload()" class="myr_retry_button">Retry</button>
      </div>
    </div>
  `;
}

function detectOS() {
  const ua = navigator.userAgent.toLowerCase();
  const platform = navigator.platform?.toLowerCase() || '';

  // Check mobile/tablet FIRST (iPadOS reports as macOS in Safari)
  if (ua.includes('iphone') || ua.includes('ipad')) return 'ios';
  // Also detect iPad via touch + macOS combination (iPadOS 13+ desktop mode)
  if (navigator.maxTouchPoints > 1 && (ua.includes('mac') || platform.includes('mac'))) return 'ios';
  if (ua.includes('android')) return 'android';

  // Desktop OS detection
  if (ua.includes('win') || platform.includes('win')) return 'windows';
  if (ua.includes('mac') || platform.includes('mac')) return 'macos';
  if (ua.includes('linux') || platform.includes('linux')) return 'linux';
  return 'unknown';
}

function detectBrowser() {
  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes('edg/')) return 'edge';
  if (ua.includes('chrome')) return 'chrome';
  if (ua.includes('firefox')) return 'firefox';
  if (ua.includes('safari') && !ua.includes('chrome')) return 'safari';
  return 'unknown';
}

function getSecureContextOptionsHTML(browser, os, port) {
  const origin = window.location.origin;
  const isMobile = (os === 'ios' || os === 'android');

  let options = '';

  // Only show localhost option for desktop
  if (!isMobile) {
    options += `
      <p><strong>Option 1 (easiest):</strong> Access via localhost instead:</p>
      <div class="myr_code_instructions">
        <p><code>http://localhost:${port}</code> or <code>http://127.0.0.1:${port}</code></p>
        <p class="myr_note">Browsers treat localhost as a secure context</p>
      </div>
    `;
  }

  const optNum = isMobile ? 1 : 2;
  options += `
    <p><strong>Option ${optNum}:</strong> Add this site to browser exceptions:</p>
    ${getInsecureOriginHTML(browser, os)}
    <p><strong>Option ${optNum + 1}:</strong> Use HTTPS (requires server configuration)</p>
  `;

  return options;
}

function getInsecureOriginHTML(browser, os) {
  const origin = window.location.origin;
  const hostname = window.location.hostname;

  // iOS Safari has no way to add insecure origin exceptions
  if (os === 'ios') {
    return `
      <div class="myr_code_instructions">
        <p>Safari on iOS/iPadOS does not support insecure origin exceptions.</p>
        <p>Alternatives:</p>
        <p>• Configure HTTPS on your SignalK server</p>
        <p>• Use a tunneling service (e.g., ngrok) to get an HTTPS URL</p>
        <p>• Access from a desktop browser where you can set the flag</p>
      </div>
    `;
  }

  // Android Chrome
  if (os === 'android' && browser === 'chrome') {
    return `
      <div class="myr_code_instructions">
        <p>1. Open Chrome on your Android device</p>
        <p>2. Go to: <code>chrome://flags/#unsafely-treat-insecure-origin-as-secure</code></p>
        <p>3. Add: <code>${origin}</code></p>
        <p>4. Set to "Enabled"</p>
        <p>5. Tap "Relaunch"</p>
      </div>
    `;
  }

  if (browser === 'chrome' || browser === 'edge') {
    const flagPrefix = browser === 'edge' ? 'edge' : 'chrome';
    const flagUrl = `${flagPrefix}://flags/#unsafely-treat-insecure-origin-as-secure`;
    return `
      <div class="myr_code_instructions">
        <p>1. Copy and paste this into your address bar:</p>
        <p><a href="${flagUrl}" class="myr_flag_link"><code>${flagUrl}</code></a></p>
        <p>2. In the text field, add: <code>${origin}</code></p>
        <p>3. Set dropdown to "Enabled"</p>
        <p>4. Click "Relaunch" at the bottom</p>
      </div>
    `;
  }
  if (browser === 'firefox') {
    return `
      <div class="myr_code_instructions">
        <p>1. Open: <a href="about:config" class="myr_flag_link"><code>about:config</code></a></p>
        <p>2. Click "Accept the Risk and Continue"</p>
        <p>3. Search for: <code>dom.securecontext.allowlist</code></p>
        <p>4. Click the + button to add: <code>${hostname}</code></p>
        <p>5. Restart Firefox</p>
      </div>
    `;
  }
  return `<p>Check your browser settings for allowing insecure origins.</p>`;
}

function getBrowserInstructionsHTML(browser, os) {
  // iOS/iPadOS Safari
  if (browser === 'safari' && os === 'ios') {
    return `
      <div class="myr_code_instructions">
        <p>Safari on iOS/iPadOS 17+:</p>
        <p>1. Open the <strong>Settings</strong> app</p>
        <p>2. Scroll down and tap <strong>Safari</strong></p>
        <p>3. Scroll down and tap <strong>Advanced</strong></p>
        <p>4. Tap <strong>Feature Flags</strong></p>
        <p>5. Enable <strong>WebGPU</strong></p>
        <p>6. Return to Safari and reload this page</p>
        <p class="myr_note">Note: Requires iOS/iPadOS 17 or later.</p>
      </div>
    `;
  }

  switch (browser) {
    case 'chrome':
      return `
        <div class="myr_code_instructions">
          <p>Chrome should have WebGPU enabled by default (v113+).</p>
          <p>If not working:</p>
          <p>1. Open: <code>chrome://flags/#enable-unsafe-webgpu</code></p>
          <p>2. Set to "Enabled"</p>
          <p>3. Relaunch Chrome</p>
          ${os === 'linux' ? '<p class="myr_note">Linux: Vulkan drivers required.</p>' : ''}
        </div>
      `;
    case 'edge':
      return `
        <div class="myr_code_instructions">
          <p>Edge should have WebGPU enabled by default.</p>
          <p>If not working:</p>
          <p>1. Open: <code>edge://flags/#enable-unsafe-webgpu</code></p>
          <p>2. Set to "Enabled"</p>
          <p>3. Relaunch Edge</p>
        </div>
      `;
    case 'firefox':
      return `
        <div class="myr_code_instructions">
          <p>Firefox WebGPU (experimental):</p>
          <p>1. Open: <code>about:config</code></p>
          <p>2. Search: <code>dom.webgpu.enabled</code></p>
          <p>3. Set to: <code>true</code></p>
          <p>4. Restart Firefox</p>
        </div>
      `;
    case 'safari':
      return `
        <div class="myr_code_instructions">
          <p>Safari WebGPU (macOS 14+):</p>
          <p>1. Open Safari menu > Settings</p>
          <p>2. Go to Advanced tab</p>
          <p>3. Check "Show features for web developers"</p>
          <p>4. Go to Feature Flags tab</p>
          <p>5. Enable "WebGPU"</p>
          <p>6. Restart Safari</p>
        </div>
      `;
    default:
      return `
        <div class="myr_code_instructions">
          <p>WebGPU requires:</p>
          <p>- Chrome 113+ (recommended)</p>
          <p>- Edge 113+</p>
          <p>- Safari 17+</p>
          <p>- Firefox (experimental)</p>
        </div>
      `;
  }
}

function getHardwareAccelerationHTML(browser, os) {
  // iOS/iPadOS - no hardware acceleration toggle
  if (os === 'ios') {
    return `
      <div class="myr_code_instructions">
        <p>On iOS/iPadOS, hardware acceleration cannot be disabled.</p>
        <p>If WebGPU is not working:</p>
        <p>• Ensure you have iOS/iPadOS 17 or later</p>
        <p>• Try closing and reopening Safari</p>
        <p>• Restart your device</p>
      </div>
    `;
  }

  switch (browser) {
    case 'chrome':
      return `
        <div class="myr_code_instructions">
          <p>1. Open: <code>chrome://settings/system</code></p>
          <p>2. Enable "Use graphics acceleration when available"</p>
          <p>3. Relaunch Chrome</p>
        </div>
      `;
    case 'edge':
      return `
        <div class="myr_code_instructions">
          <p>1. Open: <code>edge://settings/system</code></p>
          <p>2. Enable "Use graphics acceleration when available"</p>
          <p>3. Relaunch Edge</p>
        </div>
      `;
    case 'firefox':
      return `
        <div class="myr_code_instructions">
          <p>1. Open: <code>about:preferences</code></p>
          <p>2. Scroll to "Performance"</p>
          <p>3. Uncheck "Use recommended performance settings"</p>
          <p>4. Check "Use hardware acceleration when available"</p>
          <p>5. Restart Firefox</p>
        </div>
      `;
    case 'safari':
      return `
        <div class="myr_code_instructions">
          <p>Safari uses hardware acceleration by default on macOS.</p>
          <p>If WebGPU is not working:</p>
          <p>• Ensure you have macOS 14 (Sonoma) or later</p>
          <p>• Check that WebGPU is enabled in Feature Flags</p>
          <p>• Try restarting Safari</p>
        </div>
      `;
    default:
      return `
        <div class="myr_code_instructions">
          <p>Check your browser settings for "Hardware acceleration"</p>
          <p>or "Use GPU" and ensure it is enabled.</p>
          <p>Then restart the browser.</p>
        </div>
      `;
  }
}

function restart(id) {
  setTimeout(loadRadar, 15000, id);
}

function radarLoaded(r) {
  let maxSpokeLen = r.maxSpokeLen;
  let spokesPerRevolution = r.spokesPerRevolution;
  let prev_angle = -1;

  if (r === undefined || r.controls === undefined) {
    return;
  }
  renderer.setLegend(expandLegend(r.legend));
  renderer.setSpokes(spokesPerRevolution, maxSpokeLen);

  // Use provided streamUrl or construct SignalK stream URL
  let streamUrl = r.streamUrl;
  if (!streamUrl || streamUrl === "undefined" || streamUrl === "null") {
    const wsProtocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    streamUrl = `${wsProtocol}//${window.location.host}/signalk/v2/api/vessels/self/radars/${r.id}/stream`;
  }
  console.log("Connecting to radar stream:", streamUrl);
  webSocket = new WebSocket(streamUrl);
  webSocket.binaryType = "arraybuffer";

  webSocket.onopen = (e) => {
    console.log("websocket open: " + JSON.stringify(e));
  };
  webSocket.onclose = (e) => {
    console.log("websocket close: code=" + e.code + ", reason=" + e.reason + ", wasClean=" + e.wasClean);
    restart(r.id);
  };
  webSocket.onerror = (e) => {
    console.log("websocket error:", e);
  };
  webSocket.onmessage = (e) => {
    if (RadarMessage) {
      let buf = e.data;
      let bytes = new Uint8Array(buf);
      var message = RadarMessage.decode(bytes);
      if (message.spokes) {
        for (let i = 0; i < message.spokes.length; i++) {
          let spoke = message.spokes[i];

          // Gap-filling disabled for high spoke counts (8192) - not needed
          // The texture-based renderers handle sparse data well
          renderer.drawSpoke(spoke);
          prev_angle = spoke.angle;
          // Update range display from spoke data
          if (spoke.range) {
            setCurrentRange(spoke.range);
          }
        }
        renderer.render();
      }
    }
  };
}

function expandLegend(legend) {
  let a = Array();
  for (let i = 0; i < Object.keys(legend).length; i++) {
    let color = legend[i].color;
    a.push(hexToRGBA(color));
  }
  a[0][3] = 255;

  return a;
}

function hexToRGBA(hex) {
  let a = Array();
  for (let i = 1; i < hex.length; i += 2) {
    a.push(parseInt(hex.slice(i, i + 2), 16));
  }
  while (a.length < 3) {
    a.push(0);
  }
  while (a.length < 4) {
    a.push(255);
  }

  return a;
}

function controlUpdate(control, controlValue) {
  if (control.name == "Range") {
    let range = parseFloat(controlValue.value);
    renderer.setRange(range);
  }
  if (control.name.startsWith("No Transmit")) {
    let value = parseFloat(controlValue.value);
    let idx = extractNoTxZone(control.name);
    let start_or_end = extractStartOrEnd(control.name);
    if (controlValue.enabled) {
      noTransmitAngles[idx][start_or_end] = value;
    } else {
      noTransmitAngles[idx] = null;
    }
  }
}

function extractNoTxZone(name) {
  const re = /(\d+)/;
  let match = name.match(re);
  if (match) {
    return parseInt(match[1]);
  }
  return 0;
}

function extractStartOrEnd(name) {
  return name.includes("start") ? 0 : 1;
}

function drawBackground(obj, txt) {
  obj.background_ctx.setTransform(1, 0, 0, 1, 0, 0);
  obj.background_ctx.clearRect(0, 0, obj.width, obj.height);

  // No transmit zones (drawn on background, behind radar)
  obj.background_ctx.fillStyle = "lightgrey";
  if (typeof noTransmitAngles == "array") {
    noTransmitAngles.forEach((e) => {
      if (e && e[0]) {
        obj.background_ctx.beginPath();
        obj.background_ctx.arc(
          obj.center_x,
          obj.center_y,
          obj.beam_length * 2,
          (2 * Math.PI * e[0]) / obj.spokesPerRevolution,
          (2 * Math.PI * e[1]) / obj.spokesPerRevolution
        );
        obj.background_ctx.fill();
      }
    });
  }

  // Title text
  obj.background_ctx.fillStyle = "lightblue";
  obj.background_ctx.font = "bold 16px/1 Verdana, Geneva, sans-serif";
  obj.background_ctx.fillText(txt, 5, 20);
}
