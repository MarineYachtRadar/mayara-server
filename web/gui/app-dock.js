// App Dock, the Signal K webapp switcher, opens on a double-tap anywhere, which
// it notices by listening inside the page it shows. A browser allows that only
// for pages on App Dock's own origin, and this GUI opened directly on mayara's
// port is on another one, so a double-tap over the radar would go unseen.
// Forwarding each pointerdown to the top window lets App Dock count it. The
// message carries only the pointer position and when the tap happened: while
// the radar renders, a message can arrive late enough to split a double-tap
// if App Dock had to go by its arrival.
const APP_DOCK_POINTERDOWN = "signalk-app-dock:pointerdown";

if (window.self !== window.top) {
  document.addEventListener(
    "pointerdown",
    (e) => {
      window.top.postMessage(
        {
          type: APP_DOCK_POINTERDOWN,
          x: e.clientX,
          y: e.clientY,
          t: performance.timeOrigin + e.timeStamp,
        },
        "*",
      );
    },
    { passive: true, capture: true },
  );
}
