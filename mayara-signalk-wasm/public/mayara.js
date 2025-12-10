import van from "./van-1.5.2.debug.js";
import { fetchRadars, fetchInterfaces, isStandaloneMode, detectMode } from "./api.js";

const { a, tr, td, div, p, strong, details, summary, code, br, span } = van.tags;

// Network requirements for different radar brands
const NETWORK_REQUIREMENTS = {
  furuno: {
    ipRange: "172.31.x.x/16",
    description: "Furuno DRS radars require the host to have an IP address in the 172.31.x.x range.",
    setup: [
      "Configure your network interface with an IP like 172.31.3.100/16",
      "Connect to the radar network (usually via ethernet)",
      "Ensure no firewall blocks UDP ports 10010, 10024, 10021"
    ],
    example: "ip addr add 172.31.3.100/16 dev eth1"
  },
  navico: {
    ipRange: "236.6.7.x (multicast)",
    description: "Navico (Simrad/Lowrance/B&G) radars use multicast.",
    setup: ["Ensure your network supports multicast routing"]
  },
  raymarine: {
    ipRange: "232.1.1.x (multicast)",
    description: "Raymarine radars use multicast.",
    setup: ["Ensure your network supports multicast routing"]
  },
  garmin: {
    ipRange: "239.254.2.x (multicast)",
    description: "Garmin xHD radars use multicast.",
    setup: ["Ensure your network supports multicast routing"]
  }
};

// Detect operating system
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

// Detect browser
function detectBrowser() {
  const ua = navigator.userAgent.toLowerCase();

  if (ua.includes('edg/')) return 'edge';
  if (ua.includes('chrome')) return 'chrome';
  if (ua.includes('firefox')) return 'firefox';
  if (ua.includes('safari') && !ua.includes('chrome')) return 'safari';
  return 'unknown';
}

// Check if using a secure context
// Note: localhost and 127.0.0.1 are treated as secure contexts by browsers
function isSecureContext() {
  return window.isSecureContext;
}

// Check WebGPU support and show appropriate warnings
async function checkWebGPUSupport() {
  const warningDiv = document.getElementById('webgpu_warning');
  if (!warningDiv) return true;

  const os = detectOS();
  const browser = detectBrowser();
  const isSecure = isSecureContext();
  const hasWebGPUApi = !!navigator.gpu;

  // Track why WebGPU failed
  let failureReason = null;
  let hasWorkingAdapter = false;

  if (hasWebGPUApi) {
    try {
      const adapter = await navigator.gpu.requestAdapter();
      if (adapter) {
        // WebGPU fully working!
        warningDiv.style.display = 'none';
        return true;
      } else {
        failureReason = 'no-adapter'; // API exists but no GPU adapter found
      }
    } catch (e) {
      console.warn('WebGPU adapter request failed:', e);
      failureReason = 'adapter-error';
    }
  } else {
    // API doesn't exist - could be secure context issue or browser doesn't support it
    failureReason = isSecure ? 'no-api' : 'insecure-context';
  }

  // Build warning message based on situation
  warningDiv.style.display = 'block';
  warningDiv.innerHTML = '';

  const title = div({ class: 'myr_warning_title' }, 'WebGPU Not Available');
  van.add(warningDiv, title);

  const content = div({ class: 'myr_warning_content' });
  van.add(warningDiv, content);

  // Secure context warning (if not secure)
  if (!isSecure) {
    const hostname = window.location.hostname;
    const port = window.location.port || '80';
    const isMobile = (os === 'ios' || os === 'android');

    van.add(content, div({ class: 'myr_warning_item myr_warning_https' },
      strong('Secure Context Required'),
      p('WebGPU requires a secure context. You are currently using HTTP on "', hostname, '".'),
      p('Options:'),
      div({ class: 'myr_warning_options' },
        // Only show localhost option for desktop (SignalK won't run on mobile)
        !isMobile ? div({ class: 'myr_warning_option' },
          strong('Option 1 (easiest): '), 'Access via localhost instead:',
          div({ class: 'myr_code_block' },
            p(code('http://localhost:' + port), ' or ', code('http://127.0.0.1:' + port)),
            p({ class: 'myr_note' }, 'Browsers treat localhost as a secure context')
          )
        ) : null,
        div({ class: 'myr_warning_option' },
          strong(isMobile ? 'Option 1: ' : 'Option 2: '), 'Add this site to browser exceptions:',
          getInsecureOriginInstructions(browser, os)
        ),
        div({ class: 'myr_warning_option' },
          strong(isMobile ? 'Option 2: ' : 'Option 3: '), 'Use HTTPS (requires server configuration)'
        )
      )
    ));
  }

  // Always show browser-specific WebGPU/hardware acceleration instructions
  van.add(content, div({ class: 'myr_warning_item' },
    strong('Enable WebGPU / Hardware Acceleration'),
    getBrowserInstructions(browser, os)
  ));

  return false;
}

function getInsecureOriginInstructions(browser, os) {
  const origin = window.location.origin;

  // iOS Safari has no way to add insecure origin exceptions
  if (os === 'ios') {
    return div({ class: 'myr_code_block' },
      p('Safari on iOS/iPadOS does not support insecure origin exceptions.'),
      p('Alternatives:'),
      p('• Configure HTTPS on your SignalK server'),
      p('• Use a tunneling service (e.g., ngrok) to get an HTTPS URL'),
      p('• Access from a desktop browser where you can set the flag')
    );
  }

  // Android Chrome
  if (os === 'android' && browser === 'chrome') {
    return div({ class: 'myr_code_block' },
      p('1. Open Chrome on your Android device'),
      p('2. Go to: ', code('chrome://flags/#unsafely-treat-insecure-origin-as-secure')),
      p('3. Add: ', code(origin)),
      p('4. Set to "Enabled"'),
      p('5. Tap "Relaunch"')
    );
  }

  switch (browser) {
    case 'chrome':
    case 'edge':
      const flagPrefix = browser === 'edge' ? 'edge' : 'chrome';
      const flagUrl = `${flagPrefix}://flags/#unsafely-treat-insecure-origin-as-secure`;
      return div({ class: 'myr_code_block' },
        p('1. Copy and paste this into your address bar:'),
        p(a({ href: flagUrl, class: 'myr_flag_link' }, code(flagUrl))),
        p('2. In the text field, add: ', code(origin)),
        p('3. Set dropdown to "Enabled"'),
        p('4. Click "Relaunch" at the bottom')
      );
    case 'firefox':
      return div({ class: 'myr_code_block' },
        p('1. Open: ', a({ href: 'about:config', class: 'myr_flag_link' }, code('about:config'))),
        p('2. Click "Accept the Risk and Continue"'),
        p('3. Search for: ', code('dom.securecontext.allowlist')),
        p('4. Click the + button to add: ', code(window.location.hostname)),
        p('5. Restart Firefox')
      );
    default:
      return div({ class: 'myr_code_block' },
        p('Check your browser settings for allowing insecure origins.')
      );
  }
}

function getBrowserInstructions(browser, os) {
  // iOS/iPadOS Safari
  if (browser === 'safari' && os === 'ios') {
    return div({ class: 'myr_code_block' },
      p('Safari on iOS/iPadOS 17+:'),
      p('1. Open ', strong('Settings'), ' app'),
      p('2. Scroll down and tap ', strong('Safari')),
      p('3. Scroll down and tap ', strong('Advanced')),
      p('4. Tap ', strong('Feature Flags')),
      p('5. Enable ', strong('WebGPU')),
      p('6. Return to Safari and reload this page'),
      p({ class: 'myr_note' }, 'Note: Requires iOS/iPadOS 17 or later.')
    );
  }

  switch (browser) {
    case 'chrome':
      return div({ class: 'myr_code_block' },
        p('Chrome should have WebGPU enabled by default (v113+).'),
        p('If not working, try:'),
        p('1. Open: ', code('chrome://flags/#enable-unsafe-webgpu')),
        p('2. Set to "Enabled"'),
        p('3. Relaunch Chrome'),
        os === 'linux' ? p({ class: 'myr_note' },
          'Note: On Linux, you may need Vulkan drivers installed.') : null
      );
    case 'edge':
      return div({ class: 'myr_code_block' },
        p('Edge should have WebGPU enabled by default.'),
        p('If not working, try:'),
        p('1. Open: ', code('edge://flags/#enable-unsafe-webgpu')),
        p('2. Set to "Enabled"'),
        p('3. Relaunch Edge')
      );
    case 'firefox':
      return div({ class: 'myr_code_block' },
        p('Firefox WebGPU is experimental:'),
        p('1. Open: ', code('about:config')),
        p('2. Search for: ', code('dom.webgpu.enabled')),
        p('3. Set to: ', code('true')),
        p('4. Restart Firefox'),
        p({ class: 'myr_note' }, 'Note: Firefox WebGPU support is still in development.')
      );
    case 'safari':
      return div({ class: 'myr_code_block' },
        p('Safari WebGPU (macOS 14+):'),
        p('1. Open Safari menu > Settings'),
        p('2. Go to Advanced tab'),
        p('3. Check "Show features for web developers"'),
        p('4. Go to Feature Flags tab'),
        p('5. Enable "WebGPU"'),
        p('6. Restart Safari')
      );
    default:
      return div({ class: 'myr_code_block' },
        p('WebGPU requires a modern browser:'),
        p('- Chrome 113+ (recommended)'),
        p('- Edge 113+'),
        p('- Safari 17+ (macOS/iOS)'),
        p('- Firefox Nightly (experimental)')
      );
  }
}

function getHardwareAccelerationInstructions(browser, os) {
  // iOS/iPadOS - no hardware acceleration toggle
  if (os === 'ios') {
    return div({ class: 'myr_code_block' },
      p('On iOS/iPadOS, hardware acceleration cannot be disabled.'),
      p('If WebGPU is not working:'),
      p('• Ensure you have iOS/iPadOS 17 or later'),
      p('• Try closing and reopening Safari'),
      p('• Restart your device')
    );
  }

  switch (browser) {
    case 'chrome':
      return div({ class: 'myr_code_block' },
        p('1. Open: ', code('chrome://settings/system')),
        p('2. Enable "Use graphics acceleration when available"'),
        p('3. Relaunch Chrome')
      );
    case 'edge':
      return div({ class: 'myr_code_block' },
        p('1. Open: ', code('edge://settings/system')),
        p('2. Enable "Use graphics acceleration when available"'),
        p('3. Relaunch Edge')
      );
    case 'firefox':
      return div({ class: 'myr_code_block' },
        p('1. Open: ', code('about:preferences')),
        p('2. Scroll to "Performance"'),
        p('3. Uncheck "Use recommended performance settings"'),
        p('4. Check "Use hardware acceleration when available"'),
        p('5. Restart Firefox')
      );
    case 'safari':
      return div({ class: 'myr_code_block' },
        p('Safari uses hardware acceleration by default on macOS.'),
        p('If WebGPU is not working:'),
        p('• Ensure you have macOS 14 (Sonoma) or later'),
        p('• Check that WebGPU is enabled in Feature Flags'),
        p('• Try restarting Safari')
      );
    default:
      return div({ class: 'myr_code_block' },
        p('Check your browser settings for "Hardware acceleration"'),
        p('or "Use GPU" and ensure it is enabled.'),
        p('Then restart the browser.')
      );
  }
}

const RadarEntry = (id, name) =>
  tr({ class: 'myr_radar_row' },
    td({ class: 'myr_radar_name' }, name),
    td({ class: 'myr_radar_actions' },
      a({ href: "viewer.html?id=" + id, class: 'myr_radar_link myr_radar_link_primary' },
        'Open Radar Display'
      ),
      a({ href: "control.html?id=" + id, class: 'myr_radar_link myr_radar_link_secondary' },
        'Controls Only'
      )
    )
  );

function radarsLoaded(d) {
  let radarIds = Object.keys(d);
  let c = radarIds.length;
  let r = document.getElementById("radars");

  // Clear previous content
  r.innerHTML = "";

  if (c > 0) {
    van.add(r, div({ class: 'myr_section_title' },
      span({ class: 'myr_radar_count' }, c),
      ' Radar' + (c > 1 ? 's' : '') + ' Detected'
    ));

    let table = document.createElement("table");
    table.className = 'myr_radar_table';
    r.appendChild(table);

    radarIds.sort().forEach(function (v, i) {
      van.add(table, RadarEntry(v, d[v].name));
    });

    // Radar found, poll less frequently
    setTimeout(loadRadars, 15000);
  } else {
    van.add(r, div({ class: 'myr_detecting' },
      span({ class: 'myr_pulse' }),
      'Searching for radars...'
    ));

    // Show network requirements help
    van.add(r,
      details({ class: 'myr_network_help' },
        summary('Network Configuration Help'),
        div({ class: 'myr_help_content' },
          // Furuno section
          div({ class: 'myr_brand_section' },
            div({ class: 'myr_brand_header' }, 'Furuno DRS (DRS4D-NXT, DRS6A-NXT, etc.)'),
            p(NETWORK_REQUIREMENTS.furuno.description),
            div({ class: 'myr_setup_steps' },
              NETWORK_REQUIREMENTS.furuno.setup.map((step, i) =>
                div({ class: 'myr_setup_step' }, (i + 1) + '. ' + step)
              )
            ),
            div({ class: 'myr_code_example' },
              code(NETWORK_REQUIREMENTS.furuno.example)
            )
          ),

          // Other brands
          div({ class: 'myr_brand_section myr_brand_other' },
            div({ class: 'myr_brand_header' }, 'Navico (Simrad, Lowrance, B&G)'),
            p(NETWORK_REQUIREMENTS.navico.description)
          ),

          div({ class: 'myr_brand_section myr_brand_other' },
            div({ class: 'myr_brand_header' }, 'Raymarine'),
            p(NETWORK_REQUIREMENTS.raymarine.description)
          ),

          div({ class: 'myr_brand_section myr_brand_other' },
            div({ class: 'myr_brand_header' }, 'Garmin xHD'),
            p(NETWORK_REQUIREMENTS.garmin.description)
          )
        )
      )
    );

    // No radar found, poll more frequently (every 2 seconds)
    setTimeout(loadRadars, 2000);
  }
}

function interfacesLoaded(d) {
  if (!d || !d.interfaces) {
    return;
  }

  let c = Object.keys(d.interfaces).length;
  if (c > 0) {
    let r = document.getElementById("interfaces");
    r.innerHTML = "";

    van.add(r, div({ class: 'myr_section_title' }, 'Network Interfaces'));

    let table = document.createElement("table");
    table.className = 'myr_interface_table';
    r.appendChild(table);

    let brands = ["Interface", ...d.brands];
    let hdr = van.add(table, tr({ class: 'myr_interface_header' }));
    brands.forEach((v) => van.add(hdr, td(v)));

    let interfaces = d.interfaces;
    if (interfaces) {
      console.log("interfaces", interfaces);
      Object.keys(interfaces).forEach(function (v, i) {
        let row = van.add(table, tr());

        van.add(row, td({ class: 'myr_interface_name' }, v));
        if (interfaces[v].status) {
          van.add(row, td({ class: 'myr_interface_error', colspan: d.brands.length }, interfaces[v].status));
        } else {
          d.brands.forEach((b) => {
            let status = interfaces[v].listeners[b];
            let className = (status == "Listening" || status == "Active")
              ? 'myr_interface_ok'
              : 'myr_interface_error';
            van.add(row, td({ class: className }, status));
          });
        }
      });
    }
  }
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

async function loadInterfaces() {
  try {
    const interfaces = await fetchInterfaces();
    if (interfaces) {
      interfacesLoaded(interfaces);
    } else {
      // Hide interfaces section in SignalK mode
      let r = document.getElementById("interfaces");
      if (r) {
        r.style.display = "none";
      }
    }
  } catch (err) {
    console.error("Failed to load interfaces:", err);
  }
}

window.onload = async function () {
  // Check WebGPU support first
  await checkWebGPUSupport();

  // Detect mode
  await detectMode();

  // Load data
  loadRadars();
  loadInterfaces();
};
