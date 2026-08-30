import van from "./vendor/van-1.5.2.debug.js";
import {
  fetchRadars,
  fetchInterfaces,
  getDiagnosticsUrl,
  isStandaloneMode,
  detectMode,
  apiFetch,
  fetchServerStatus,
  fetchTelemetryConsent,
  setTelemetryConsent,
} from "./api.js";
import { radarCombinations, multiViewUrl } from "./radar-list.js";
import { HELP_AFTER_MS, renderSearchHelp } from "./search-help.js";

const { a, tr, td, div, p, strong, details, summary, code, br, span, button } =
  van.tags;

// Global WebGPU availability flag
let webGPUAvailable = false;

// When the current run of finding nothing began, and what the server told us
// about radars seen before. Both feed the help offered after HELP_AFTER_MS.
let searchingSince = Date.now();
let knownRadars = [];
let compiledBrands = [];

// Network requirements for different radar brands
const NETWORK_REQUIREMENTS = {
  furuno: {
    ipRange: "172.31.x.x/16",
    description:
      "Furuno DRS radars require the host to have an IP address in the 172.31.x.x range.",
    setup: [
      "Configure your network interface with an IP like 172.31.3.100/16",
      "Connect to the radar network (usually via ethernet)",
      "Ensure no firewall blocks UDP ports 10010, 10024, 10021",
    ],
    example: "ip addr add 172.31.3.100/16 dev eth1",
  },
  navico: {
    ipRange: "236.6.7.x (multicast)",
    description: "Navico (Simrad/Lowrance/B&G) radars use multicast.",
    setup: ["Ensure your network supports multicast routing"],
  },
  raymarine: {
    ipRange: "232.1.1.x (multicast)",
    description:
      "Raymarine radars use multicast, and they get their IP address by DHCP. " +
      "The radar network needs a DHCP server — an MFD, a router, or a DHCP " +
      "service on this machine — or the radar never announces itself. A wired " +
      "Quantum needs no pairing and no WiFi credentials.",
    setup: ["Ensure your network supports multicast routing"],
  },
  garmin: {
    ipRange: "239.254.2.x (multicast)",
    description:
      "Garmin radars use multicast and require the host to have an IP address " +
      "in the 172.16.x.x - 172.31.x.x range.",
    setup: ["Ensure your network supports multicast routing"],
  },
  koden: {
    ipRange: "255.255.255.255:10001 (broadcast)",
    description:
      "Koden radars use UDP broadcast on port 10001. The host must be on the " +
      "same subnet as the radar, typically 192.168.0.x.",
    setup: ["Ensure UDP port 10001 is not blocked"],
  },
};

// Brands whose network help is a single paragraph plus a link to the full
// guide. Furuno is rendered separately because it also carries setup steps.
const OTHER_BRANDS = [
  ["navico", "Navico (Simrad, Lowrance, B&G)"],
  ["raymarine", "Raymarine"],
  ["garmin", "Garmin"],
  ["koden", "Koden"],
];

const brandName = (brand) => brand[0].toUpperCase() + brand.slice(1);

// Detect operating system
function detectOS() {
  const ua = navigator.userAgent.toLowerCase();
  const platform = navigator.platform?.toLowerCase() || "";

  // Check mobile/tablet FIRST (iPadOS reports as macOS in Safari)
  if (ua.includes("iphone") || ua.includes("ipad")) return "ios";
  // Also detect iPad via touch + macOS combination (iPadOS 13+ desktop mode)
  if (
    navigator.maxTouchPoints > 1 &&
    (ua.includes("mac") || platform.includes("mac"))
  )
    return "ios";
  if (ua.includes("android")) return "android";

  // Desktop OS detection
  if (ua.includes("win") || platform.includes("win")) return "windows";
  if (ua.includes("mac") || platform.includes("mac")) return "macos";
  if (ua.includes("linux") || platform.includes("linux")) return "linux";
  return "unknown";
}

// Detect browser
function detectBrowser() {
  const ua = navigator.userAgent.toLowerCase();

  if (ua.includes("edg/")) return "edge";
  if (ua.includes("chrome")) return "chrome";
  if (ua.includes("firefox")) return "firefox";
  if (ua.includes("safari") && !ua.includes("chrome")) return "safari";
  return "unknown";
}

// Check if using a secure context
// Note: localhost and 127.0.0.1 are treated as secure contexts by browsers
function isSecureContext() {
  return window.isSecureContext;
}

// Check WebGPU support and update global flag
async function checkWebGPUSupport() {
  const hasWebGPUApi = !!navigator.gpu;

  if (hasWebGPUApi) {
    try {
      const adapter = await navigator.gpu.requestAdapter();
      if (adapter) {
        webGPUAvailable = true;
        return true;
      }
    } catch (e) {
      console.warn("WebGPU adapter request failed:", e);
    }
  }

  webGPUAvailable = false;
  return false;
}

// Show WebGPU warning in the info section
function showWebGPUWarning() {
  const warningDiv = document.getElementById("webgpu_warning");
  if (!warningDiv) return;

  const os = detectOS();
  const browser = detectBrowser();
  const isSecure = isSecureContext();

  warningDiv.style.display = "block";
  warningDiv.innerHTML = "";

  const title = div({ class: "myr_warning_title" }, "WebGPU Not Available");
  van.add(warningDiv, title);

  van.add(
    warningDiv,
    p(
      { class: "myr_warning_subtitle" },
      "The preferred rendering method (WebGPU) is not available. Opening the display will use the alternate method (WebGL)."
    )
  );

  const content = div({ class: "myr_warning_content" });
  van.add(warningDiv, content);

  // Secure context warning (if not secure)
  if (!isSecure) {
    const hostname = window.location.hostname;
    const port = window.location.port || "80";
    const isMobile = os === "ios" || os === "android";

    van.add(
      content,
      div(
        { class: "myr_warning_item myr_warning_https" },
        strong("Secure Context Required"),
        p(
          'WebGPU requires a secure context. You are currently using HTTP on "',
          hostname,
          '".'
        ),
        p("Options:"),
        div(
          { class: "myr_warning_options" },
          // Only show localhost option for desktop (SignalK won't run on mobile)
          !isMobile
            ? div(
                { class: "myr_warning_option" },
                strong("Option 1 (easiest): "),
                "Access via localhost instead:",
                div(
                  { class: "myr_code_block" },
                  p(
                    code("http://localhost:" + port),
                    " or ",
                    code("http://127.0.0.1:" + port)
                  ),
                  p(
                    { class: "myr_note" },
                    "Browsers treat localhost as a secure context"
                  )
                )
              )
            : null,
          div(
            { class: "myr_warning_option" },
            strong(isMobile ? "Option 1: " : "Option 2: "),
            "Add this site to browser exceptions:",
            getInsecureOriginInstructions(browser, os)
          ),
          div(
            { class: "myr_warning_option" },
            strong(isMobile ? "Option 2: " : "Option 3: "),
            "Use HTTPS (requires server configuration)"
          )
        )
      )
    );
  }

  // Always show browser-specific WebGPU/hardware acceleration instructions
  van.add(
    content,
    div(
      { class: "myr_warning_item" },
      strong("Enable WebGPU / Hardware Acceleration"),
      getBrowserInstructions(browser, os)
    )
  );
}

// Show info about rendering methods when WebGPU is available
function showRenderingInfo() {
  const infoDiv = document.getElementById("webgpu_warning");
  if (!infoDiv) return;

  infoDiv.style.display = "block";
  infoDiv.className = "myr_rendering_info";
  infoDiv.innerHTML = "";

  van.add(infoDiv, div({ class: "myr_info_title" }, "Rendering Options"));
  van.add(
    infoDiv,
    div(
      { class: "myr_info_content" },
      div(
        { class: "myr_render_option" },
        strong("Open Radar Display"),
        " (Recommended)",
        p(
          "Uses WebGPU for GPU-accelerated rendering. More efficient, lower CPU usage, smoother display."
        )
      ),
      div(
        { class: "myr_render_option" },
        strong("Alternate Radar Display"),
        p(
          "Uses WebGL for rendering. Compatible fallback for systems without WebGPU support."
        )
      )
    )
  );
}

function getInsecureOriginInstructions(browser, os) {
  const origin = window.location.origin;

  // iOS Safari has no way to add insecure origin exceptions
  if (os === "ios") {
    return div(
      { class: "myr_code_block" },
      p("Safari on iOS/iPadOS does not support insecure origin exceptions."),
      p("Alternatives:"),
      p("• Configure HTTPS on your SignalK server"),
      p("• Use a tunneling service (e.g., ngrok) to get an HTTPS URL"),
      p("• Access from a desktop browser where you can set the flag")
    );
  }

  // Android Chrome
  if (os === "android" && browser === "chrome") {
    return div(
      { class: "myr_code_block" },
      p("1. Open Chrome on your Android device"),
      p(
        "2. Go to: ",
        code("chrome://flags/#unsafely-treat-insecure-origin-as-secure")
      ),
      p("3. Add: ", code(origin)),
      p('4. Set to "Enabled"'),
      p('5. Tap "Relaunch"')
    );
  }

  switch (browser) {
    case "chrome":
    case "edge":
      const flagPrefix = browser === "edge" ? "edge" : "chrome";
      const flagUrl = `${flagPrefix}://flags/#unsafely-treat-insecure-origin-as-secure`;
      return div(
        { class: "myr_code_block" },
        p("1. Copy and paste this into your address bar:"),
        p(a({ href: flagUrl, class: "myr_flag_link" }, code(flagUrl))),
        p("2. In the text field, add: ", code(origin)),
        p('3. Set dropdown to "Enabled"'),
        p('4. Click "Relaunch" at the bottom')
      );
    case "firefox":
      return div(
        { class: "myr_code_block" },
        p(
          "1. Open: ",
          a(
            { href: "about:config", class: "myr_flag_link" },
            code("about:config")
          )
        ),
        p('2. Click "Accept the Risk and Continue"'),
        p("3. Search for: ", code("dom.securecontext.allowlist")),
        p("4. Click the + button to add: ", code(window.location.hostname)),
        p("5. Restart Firefox")
      );
    default:
      return div(
        { class: "myr_code_block" },
        p("Check your browser settings for allowing insecure origins.")
      );
  }
}

function getBrowserInstructions(browser, os) {
  // iOS/iPadOS Safari
  if (browser === "safari" && os === "ios") {
    return div(
      { class: "myr_code_block" },
      p("Safari on iOS/iPadOS 17+:"),
      p("1. Open ", strong("Settings"), " app"),
      p("2. Scroll down and tap ", strong("Safari")),
      p("3. Scroll down and tap ", strong("Advanced")),
      p("4. Tap ", strong("Feature Flags")),
      p("5. Enable ", strong("WebGPU")),
      p("6. Return to Safari and reload this page"),
      p({ class: "myr_note" }, "Note: Requires iOS/iPadOS 17 or later.")
    );
  }

  switch (browser) {
    case "chrome":
      return div(
        { class: "myr_code_block" },
        p("Chrome should have WebGPU enabled by default (v113+)."),
        p("If not working, try:"),
        p("1. Open: ", code("chrome://flags/#enable-unsafe-webgpu")),
        p('2. Set to "Enabled"'),
        p("3. Relaunch Chrome"),
        os === "linux"
          ? p(
              { class: "myr_note" },
              "Note: On Linux, you may need Vulkan drivers installed."
            )
          : null
      );
    case "edge":
      return div(
        { class: "myr_code_block" },
        p("Edge should have WebGPU enabled by default."),
        p("If not working, try:"),
        p("1. Open: ", code("edge://flags/#enable-unsafe-webgpu")),
        p('2. Set to "Enabled"'),
        p("3. Relaunch Edge")
      );
    case "firefox":
      return div(
        { class: "myr_code_block" },
        p("Firefox WebGPU is experimental:"),
        p("1. Open: ", code("about:config")),
        p("2. Search for: ", code("dom.webgpu.enabled")),
        p("3. Set to: ", code("true")),
        p("4. Restart Firefox"),
        p(
          { class: "myr_note" },
          "Note: Firefox WebGPU support is still in development."
        )
      );
    case "safari":
      return div(
        { class: "myr_code_block" },
        p("Safari WebGPU (macOS 14+):"),
        p("1. Open Safari menu > Settings"),
        p("2. Go to Advanced tab"),
        p('3. Check "Show features for web developers"'),
        p("4. Go to Feature Flags tab"),
        p('5. Enable "WebGPU"'),
        p("6. Restart Safari")
      );
    default:
      return div(
        { class: "myr_code_block" },
        p("WebGPU requires a modern browser:"),
        p("- Chrome 113+ (recommended)"),
        p("- Edge 113+"),
        p("- Safari 17+ (macOS/iOS)"),
        p("- Firefox Nightly (experimental)")
      );
  }
}

function getHardwareAccelerationInstructions(browser, os) {
  // iOS/iPadOS - no hardware acceleration toggle
  if (os === "ios") {
    return div(
      { class: "myr_code_block" },
      p("On iOS/iPadOS, hardware acceleration cannot be disabled."),
      p("If WebGPU is not working:"),
      p("• Ensure you have iOS/iPadOS 17 or later"),
      p("• Try closing and reopening Safari"),
      p("• Restart your device")
    );
  }

  switch (browser) {
    case "chrome":
      return div(
        { class: "myr_code_block" },
        p("1. Open: ", code("chrome://settings/system")),
        p('2. Enable "Use graphics acceleration when available"'),
        p("3. Relaunch Chrome")
      );
    case "edge":
      return div(
        { class: "myr_code_block" },
        p("1. Open: ", code("edge://settings/system")),
        p('2. Enable "Use graphics acceleration when available"'),
        p("3. Relaunch Edge")
      );
    case "firefox":
      return div(
        { class: "myr_code_block" },
        p("1. Open: ", code("about:preferences")),
        p('2. Scroll to "Performance"'),
        p('3. Uncheck "Use recommended performance settings"'),
        p('4. Check "Use hardware acceleration when available"'),
        p("5. Restart Firefox")
      );
    case "safari":
      return div(
        { class: "myr_code_block" },
        p("Safari uses hardware acceleration by default on macOS."),
        p("If WebGPU is not working:"),
        p("• Ensure you have macOS 14 (Sonoma) or later"),
        p("• Check that WebGPU is enabled in Feature Flags"),
        p("• Try restarting Safari")
      );
    default:
      return div(
        { class: "myr_code_block" },
        p('Check your browser settings for "Hardware acceleration"'),
        p('or "Use GPU" and ensure it is enabled.'),
        p("Then restart the browser.")
      );
  }
}

const RadarEntry = (radar) => {
  // Build display name: "Brand Model (Name)" or "Brand Name" if no model
  const brand = radar.brand || "";
  const model = radar.model || "";
  const name = radar.name || "";

  let displayName;
  if (model && model !== "Unknown") {
    displayName = `${brand} ${model} (${name})`;
  } else {
    displayName = `${brand} ${name}`;
  }

  const actions = [
    a(
      {
        href: "viewer.html?id=" + encodeURIComponent(radar.id),
        class: "myr_radar_link myr_radar_link_primary",
      },
      "Open Radar Display"
    ),
  ];

  // WebGL is only offered as an alternative when WebGPU is the default
  if (webGPUAvailable) {
    actions.push(
      a(
        {
          href:
            "viewer.html?id=" + encodeURIComponent(radar.id) + "&renderer=webgl",
          class: "myr_radar_link myr_radar_link_secondary",
        },
        "Alternate Display"
      )
    );
  }

  return tr(
    { class: "myr_radar_row" },
    td({ class: "myr_radar_name" }, displayName),
    td({ class: "myr_radar_actions" }, ...actions)
  );
};

// Several radars at once, in one window. The panes pick their own renderer,
// so there is no alternate-display variant to offer here.
const CombinationEntry = (combination) =>
  tr(
    { class: "myr_radar_row myr_radar_row_combined" },
    td({ class: "myr_radar_name" }, combination.label),
    td(
      { class: "myr_radar_actions" },
      a(
        {
          href: multiViewUrl(combination.ids),
          class: "myr_radar_link myr_radar_link_primary",
        },
        "Open Combined Display"
      )
    )
  );

// Track previous radar count to avoid unnecessary DOM rebuilds
let previousRadarCount = -1;

function radarsLoaded(d) {
  let radarIds = Object.keys(d);
  let c = radarIds.length;
  let r = document.getElementById("radars");

  // Only rebuild if radar count changed (avoids collapsing the help details)
  if (c === previousRadarCount && c === 0) {
    // Still nothing: the list needs no rebuild, but the help may be due.
    updateSearchHelp(c);
    setTimeout(loadRadars, 2000);
    return;
  }
  previousRadarCount = c;
  if (c > 0) {
    searchingSince = Date.now();
  }
  updateSearchHelp(c);

  // Clear previous content
  r.innerHTML = "";

  if (c > 0) {
    van.add(
      r,
      div(
        { class: "myr_section_title" },
        span({ class: "myr_radar_count" }, c),
        " Radar" + (c > 1 ? "s" : "") + " Detected"
      )
    );

    let table = document.createElement("table");
    table.className = "myr_radar_table";
    r.appendChild(table);

    // The API's own order, which the viewer's radar selector follows too and
    // which keeps the ranges of one antenna adjacent.
    const radars = radarIds.map((v) => ({ ...d[v], id: v }));
    radars.forEach((radar) => van.add(table, RadarEntry(radar)));
    radarCombinations(radars).forEach((combination) =>
      van.add(table, CombinationEntry(combination))
    );

    // Radar found, poll less frequently
    setTimeout(loadRadars, 15000);
  } else {
    van.add(
      r,
      div(
        { class: "myr_detecting" },
        span({ class: "myr_pulse" }),
        "Searching for radars..."
      )
    );

    // Show network requirements help
    van.add(
      r,
      details(
        { class: "myr_network_help" },
        summary("Network Configuration Help"),
        div(
          { class: "myr_help_content" },
          p(
            "Wired radars must be reached over wired Ethernet. ",
            a({ href: "help/networking.html" }, "Why WiFi does not carry radar data"),
            "."
          ),

          // Furuno gets the extra subnet configuration steps inline; it is by
          // far the most common reason a radar stays undetected.
          details(
            { class: "myr_brand_section" },
            summary(
              { class: "myr_brand_header" },
              "Furuno DRS (DRS4D-NXT, DRS6A-NXT, etc.)"
            ),
            p(NETWORK_REQUIREMENTS.furuno.description),
            div(
              { class: "myr_setup_steps" },
              NETWORK_REQUIREMENTS.furuno.setup.map((step, i) =>
                div({ class: "myr_setup_step" }, i + 1 + ". " + step)
              )
            ),
            div(
              { class: "myr_code_example" },
              code(NETWORK_REQUIREMENTS.furuno.example)
            ),
            p(a({ href: "help/furuno.html" }, "Full Furuno setup guide"))
          ),

          OTHER_BRANDS.map(([brand, header]) =>
            details(
              { class: "myr_brand_section myr_brand_other" },
              summary({ class: "myr_brand_header" }, header),
              p(NETWORK_REQUIREMENTS[brand].description),
              p(
                a(
                  { href: `help/${brand}.html` },
                  `Full ${brandName(brand)} setup guide`
                )
              )
            )
          )
        )
      )
    );

    // No radar found, poll more frequently (every 2 seconds)
    setTimeout(loadRadars, 2000);
  }
}

function createInterfacesModal() {
  // Create modal if it doesn't exist
  let modal = document.getElementById("interfaces_modal");
  if (modal) {
    return modal;
  }

  modal = div(
    { id: "interfaces_modal", class: "myr_interfaces_modal" },
    div(
      { class: "myr_interfaces_content" },
      div(
        { class: "myr_interfaces_header" },
        div({ class: "myr_interfaces_title" }, "Network Interfaces"),
        button(
          {
            class: "myr_interfaces_close",
            onclick: () => hideInterfacesPopup(),
          },
          "Close"
        )
      ),
      div({ id: "interfaces_modal_body" })
    )
  );

  // Close when clicking outside the content
  modal.addEventListener("click", (e) => {
    if (e.target === modal) {
      hideInterfacesPopup();
    }
  });

  document.body.appendChild(modal);
  return modal;
}

async function showInterfacesPopup() {
  const modal = createInterfacesModal();
  const body = document.getElementById("interfaces_modal_body");
  body.innerHTML = "";

  // Show loading state
  van.add(
    body,
    div(
      { class: "myr_detecting" },
      span({ class: "myr_pulse" }),
      "Loading interfaces..."
    )
  );
  modal.classList.add("myr_show");

  // Fetch fresh interface data
  try {
    const d = await fetchInterfaces();

    body.innerHTML = "";

    if (!d || !d.interfaces) {
      van.add(
        body,
        div({ class: "myr_detecting" }, "No interface data available")
      );
      return;
    }

    const interfaces = d.interfaces;
    const c = Object.keys(interfaces).length;

    if (c === 0) {
      van.add(body, div({ class: "myr_detecting" }, "No interfaces found"));
      return;
    }

    // Categorize interfaces
    const okInterfaces = [];
    const noIpInterfaces = [];
    const wirelessInterfaces = [];

    Object.keys(interfaces).forEach((name) => {
      const iface = interfaces[name];
      if (iface.status === "NoIPv4Address") {
        noIpInterfaces.push(name);
      } else if (iface.status === "WirelessIgnored") {
        wirelessInterfaces.push(name);
      } else if (iface.status === "Ok") {
        okInterfaces.push({ name, data: iface });
      }
    });

    // Show active interfaces with brand status
    if (okInterfaces.length > 0) {
      let table = document.createElement("table");
      table.className = "myr_interface_table";
      body.appendChild(table);

      let brands = ["Interface", ...d.brands];
      let hdr = van.add(table, tr({ class: "myr_interface_header" }));
      brands.forEach((v) => van.add(hdr, td(v)));

      okInterfaces.forEach(({ name, data }) => {
        let row = van.add(table, tr());
        van.add(
          row,
          td({ class: "myr_interface_name" }, name)
        );
        d.brands.forEach((b) => {
          let status = data.listeners[b];
          let className;
          if (status === "Active") {
            className = "myr_interface_ok";
          } else if (status === "Listening") {
            className = "myr_interface_listening";
          } else {
            className = "myr_interface_error";
          }
          van.add(row, td({ class: className }, status));
        });
      });
    }

    // Show wireless ignored interfaces
    if (wirelessInterfaces.length > 0) {
      van.add(
        body,
        div(
          { class: "myr_interface_ignored" },
          strong("Wireless (ignored): "),
          wirelessInterfaces.join(", ")
        )
      );
    }

    // Show no IPv4 interfaces
    if (noIpInterfaces.length > 0) {
      van.add(
        body,
        div(
          { class: "myr_interface_ignored" },
          strong("No IPv4 address: "),
          noIpInterfaces.join(", ")
        )
      );
    }

    // If no active interfaces
    if (okInterfaces.length === 0) {
      van.add(
        body,
        div({ class: "myr_detecting" }, "No active network interfaces")
      );
    }
  } catch (err) {
    console.error("Failed to load interfaces:", err);
    body.innerHTML = "";
    van.add(
      body,
      div({ class: "myr_detecting" }, "Failed to load interface data")
    );
  }
}

function hideInterfacesPopup() {
  const modal = document.getElementById("interfaces_modal");
  if (modal) {
    modal.classList.remove("myr_show");
  }
}

// Fetch the diagnostics blob via JS so we can show a busy state on the
// button instead of giving the user no feedback while the ~5 s endpoint
// runs. Mutates the button DOM directly — Van.js reactive state would
// be overkill for a single transient interaction.
async function downloadDiagnostics(btn) {
  if (btn.disabled) return;
  const restoreLabel = btn.textContent;
  btn.disabled = true;
  btn.replaceChildren(
    span({
      class: "myr_pulse",
      style:
        "display: inline-block; vertical-align: middle; margin-right: 10px;",
    }),
    document.createTextNode("Generating diagnostics…")
  );
  try {
    const resp = await apiFetch(getDiagnosticsUrl());
    if (!resp.ok) {
      throw new Error(`HTTP ${resp.status} ${resp.statusText}`);
    }
    const blob = await resp.blob();
    const filename =
      parseAttachmentFilename(resp.headers.get("Content-Disposition")) ||
      fallbackDiagnosticsFilename();
    triggerBlobDownload(blob, filename);
  } catch (err) {
    console.error("Failed to download diagnostics:", err);
    alert("Failed to generate diagnostics: " + err.message);
  } finally {
    btn.disabled = false;
    btn.replaceChildren(document.createTextNode(restoreLabel));
  }
}

// Parse the filename out of `attachment; filename="…"`. Only the quoted
// form is recognised — that's what mayara always emits.
function parseAttachmentFilename(headerValue) {
  if (!headerValue) return null;
  const m = headerValue.match(/filename="([^"]+)"/);
  return m ? m[1] : null;
}

function fallbackDiagnosticsFilename() {
  const ts = new Date()
    .toISOString()
    .replace(/[-:]/g, "")
    .replace(/\.\d+Z$/, "Z");
  return `mayara-network-diagnostics-${ts}.json.gz`;
}

function triggerBlobDownload(blob, filename) {
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  document.body.removeChild(link);
  URL.revokeObjectURL(url);
}

function showActionButtons() {
  const container = document.getElementById("action_buttons");
  if (!container) {
    return;
  }

  const buttons = [];

  if (isStandaloneMode()) {
    buttons.push(
      button(
        {
          class: "myr_radar_link myr_radar_link_secondary",
          onclick: () => showInterfacesPopup(),
        },
        "Interfaces"
      ),
      a(
        {
          href: "recordings.html",
          class: "myr_radar_link myr_radar_link_secondary",
        },
        "Recordings"
      )
    );
  }

  // Generating the diagnostics is a ~5 s blocking operation server-side
  // (ARP read + 3 s mDNS browse + 5 s passive multicast snoop, run in
  // parallel and capped by the longest leg). Without an explicit busy
  // state the button would just look broken for that whole interval, so
  // intercept the click, fetch via JS, and swap the label for a pulsing
  // "Generating diagnostics…" indicator while the request is in flight.
  buttons.push(
    button(
      {
        class: "myr_radar_link myr_radar_link_secondary",
        title:
          "Download a gzipped JSON report of the network state " +
          "(takes ~5 seconds). Attach this to a GitHub issue when " +
          "reporting that a radar is not detected.",
        onclick: (e) => downloadDiagnostics(e.currentTarget),
      },
      "Network Diagnostics"
    )
  );

  van.add(container, div({ class: "myr_action_buttons" }, ...buttons));
}

// Warn that nothing the user sets up on this server survives a restart.
// Shown on the discovery page because that is where someone lands, and the
// page holds no WebSocket to carry the matching Signal K notification.
async function showSettingsWarning() {
  const status = await fetchServerStatus();
  if (status && Array.isArray(status.knownRadars)) {
    knownRadars = status.knownRadars;
  }
  if (status && Array.isArray(status.brands)) {
    compiledBrands = status.brands;
  }
  if (!status || status.settingsStored || !status.settingsPath) return;

  const warningDiv = document.getElementById("settings_warning");
  if (!warningDiv) return;

  warningDiv.style.display = "block";
  warningDiv.replaceChildren();

  van.add(warningDiv, div({ class: "myr_warning_title" }, "Settings are not being saved"));
  van.add(
    warningDiv,
    div(
      { class: "myr_warning_content" },
      p(
        "Mayara cannot write to ",
        code(status.settingsPath),
        ", so radar names, guard zones and exclusion zones you set up now are ",
        strong("forgotten when mayara restarts"),
        "."
      ),
      p(
        "The radars themselves work normally. To keep your settings, give mayara " +
          "permission to write that file, or point it at a folder it owns."
      )
    )
  );
}

// Put the usage-report question to the user, once. The server answers
// "unasked" only while it is able to remember what they say, so a yes or no
// here is the last time they see this.
async function askAboutTelemetry() {
  const state = await fetchTelemetryConsent();
  if (!state || state.consent !== "unasked") return;

  const box = document.getElementById("telemetry_ask");
  if (!box) return;

  const answer = async (consent) => {
    box.style.display = "none";
    await setTelemetryConsent(consent);
  };

  box.style.display = "block";
  box.replaceChildren();
  van.add(box, div({ class: "myr_ask_title" }, "Inform developers of successful deploy?"));
  van.add(
    box,
    div(
      { class: "myr_ask_content" },
      p(
        "Mayara can tell its developers, at most twice, that your radar works: ",
        "once when it first delivers a picture and once when it first accepts a ",
        "setting you change. That is how we learn which radars and which computers ",
        "people actually get working."
      ),
      p(
        "A report holds the mayara version, your operating system, the brand and ",
        "model of your radar, and a random number for this installation. ",
        strong("Never"),
        " your position, your vessel, your radar's serial number or any network address."
      ),
      div(
        { class: "myr_ask_buttons" },
        button({ class: "myr_ask_yes", onclick: () => answer(true) }, "Yes, report it"),
        button({ class: "myr_ask_no", onclick: () => answer(false) }, "No thanks")
      )
    )
  );
}

// True while the help is on screen, so a poll does not re-render a dropdown
// the user is reading.
let helpShown = false;

// Offer the "what were you expecting?" help once a radar has stayed missing
// for long enough, and take it away the moment one turns up.
function updateSearchHelp(radarCount) {
  const box = document.getElementById("search_help");
  if (!box) return;

  const waited = Date.now() - searchingSince;
  const due = radarCount === 0 && waited >= HELP_AFTER_MS;

  if (!due) {
    box.style.display = "none";
    box.replaceChildren();
    helpShown = false;
    return;
  }

  // Rendering again would throw away a dropdown the user is reading.
  if (helpShown) return;

  helpShown = true;
  box.style.display = "block";
  renderSearchHelp(box, knownRadars, compiledBrands);
}

async function loadRadars() {
  try {
    const radars = await fetchRadars();
    radarsLoaded(radars);
  } catch (err) {
    console.error("Failed to load radars:", err);
    setTimeout(loadRadars, 15000);
  }
}

window.onload = async function () {
  // Check WebGPU support first
  const hasWebGPU = await checkWebGPUSupport();

  // Show appropriate info/warning based on WebGPU availability
  if (hasWebGPU) {
    showRenderingInfo();
  } else {
    showWebGPUWarning();
  }

  // Detect mode
  await detectMode();

  // Show action buttons (standalone mode only)
  showActionButtons();

  // Load data
  loadRadars();
  showSettingsWarning();
  askAboutTelemetry();

  // Hide the interfaces section (now shown via popup)
  const interfacesSection = document.getElementById("interfaces");
  if (interfacesSection) {
    interfacesSection.style.display = "none";
  }
};
