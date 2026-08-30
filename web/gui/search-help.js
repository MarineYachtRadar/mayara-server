// What the discovery page says once a radar has not turned up.
//
// The network hints below the radar list describe every brand at once, which
// is the right thing to read while you are still hopeful. After a while it
// stops being useful: the user is looking at a page that says "searching" and
// has no idea which of five brands' requirements applies to them. So we ask
// the one question that narrows it down -- what were you expecting? -- and let
// the server answer whether this host could carry it.

import van from "./vendor/van-1.5.2.debug.js";
import { fetchNetworkCheck } from "./api.js";

const { div, p, span, strong, select, option, label } = van.tags;

// How long to let discovery run before offering help. Long enough that a
// radar that is merely slow to announce itself is not preempted -- a HALO
// answers within a couple of seconds, a cold Quantum takes longer -- and short
// enough that nobody is left watching a spinner wondering if it is broken.
export const HELP_AFTER_MS = 20000;

// The question is about the radar the user owns, not about the protocol
// family mayara sorts them into, so Raymarine appears three times: the three
// products need three different networks.
export const EXPECTATIONS = [
  ["navico", "Navico", "Navico — Simrad, Lowrance, B&G (HALO, 3G/4G, BR24)"],
  ["furuno", "Furuno", "Furuno DRS (DRS4D-NXT, DRS6A-NXT, …)"],
  ["garmin", "Garmin", "Garmin (xHD, xHD2, Fantom)"],
  ["raymarine-rd", "Raymarine", "Raymarine RD (RD418HD, RD424HD, analogue via E-series)"],
  ["raymarine-quantum-mfd", "Raymarine", "Raymarine Quantum, on a network with an MFD"],
  ["raymarine-quantum-standalone", "Raymarine", "Raymarine Quantum, on its own (no MFD)"],
  ["koden", "Koden", "Koden"],
];

const PROMPT = ["", "", "Select the radar you are expecting…"];

/// The radars worth offering: brands are cargo features, and a build without
/// one will never find that radar however the network is set up. An older
/// server that does not report its brands gets the full list rather than an
/// empty one.
export function expectationsFor(brands) {
  const offered =
    Array.isArray(brands) && brands.length > 0
      ? EXPECTATIONS.filter(([, brand]) => brands.includes(brand))
      : EXPECTATIONS;

  return [PROMPT, ...offered];
}

/// What to say about the radars this install has had working before.
///
/// Someone whose radar worked last week has a different problem from someone
/// setting one up for the first time, and saying so is most of the reassurance
/// that the page is actually looking for *their* radar.
export function knownRadarsMessage(knownRadars) {
  if (!knownRadars || knownRadars.length === 0) return null;

  const named = knownRadars.map((radar) => {
    const model = radar.model ? ` (${radar.model})` : "";
    return radar.name + model;
  });

  return named.length === 1
    ? `Mayara is looking for every radar it supports, and for ${named[0]} in particular — it has worked here before.`
    : `Mayara is looking for every radar it supports, and in particular for these, which have worked here before: ${named.join(", ")}.`;
}

// Render the server's verdict about one kind of radar.
function verdict(check) {
  if (!check) {
    return div(
      { class: "myr_check_result" },
      p("Mayara could not check the network just now. Try the Network button below.")
    );
  }

  return div(
    { class: "myr_check_result " + (check.met ? "myr_check_ok" : "myr_check_bad") },
    p(strong(check.met ? "The network looks right." : "That will not work as set up.")),
    p(check.requirement),
    p(check.finding),
    check.remedy ? p(strong("What to do: "), check.remedy) : null,
    check.met
      ? p("Mayara keeps searching. If the radar still does not appear, check that it is powered and wired to this network.")
      : null
  );
}

/// Ask which radar the user is waiting for, and answer whether this host could
/// see it. `container` is emptied and filled.
export function renderSearchHelp(container, knownRadars, brands) {
  container.replaceChildren();

  const known = knownRadarsMessage(knownRadars);
  if (known) {
    van.add(container, p({ class: "myr_search_known" }, known));
  }

  const result = div({ class: "myr_check_slot" });

  // Answers arrive out of order if the user tries a second radar while the
  // first is still in flight, and the loser would overwrite the winner.
  let pending = 0;

  const dropdown = select(
    {
      class: "myr_expectation_select",
      onchange: async (e) => {
        const expectation = e.target.value;
        const generation = ++pending;

        if (!expectation) {
          result.replaceChildren();
          return;
        }
        result.replaceChildren(
          div({ class: "myr_check_result" }, p("Checking this host's network…"))
        );

        const check = await fetchNetworkCheck(expectation);
        if (generation !== pending) return;
        result.replaceChildren(verdict(check));
      },
    },
    expectationsFor(brands).map(([value, , text]) => option({ value }, text))
  );

  van.add(
    container,
    div({ class: "myr_search_title" }, "Still nothing found"),
    p(
      "Mayara has been searching for a while without finding a radar. Answering ",
      "this lets it tell you whether this computer's network can reach that radar ",
      "at all."
    ),
    label({ class: "myr_expectation_label" }, "What brand of radar were you expecting to show up?"),
    dropdown,
    result
  );
}
