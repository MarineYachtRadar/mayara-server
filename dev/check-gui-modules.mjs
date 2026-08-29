// Check that the hand-written GUI is loadable before it ships.
//
// The GUI is embedded into the binary at compile time, so nothing about the
// Rust build says whether a browser can parse it: a broken module compiles,
// ships, and fails only in the user's browser -- where a single syntax error
// takes the whole page with it and the radar list never renders.
//
// Two things are checked per module:
//   1. It parses as an ES module. `node --check <file>.js` does NOT do this --
//      node treats a .js file as a script and accepts, among other things,
//      merge conflict markers. Only `--input-type=module` rejects them.
//   2. Every name it imports from a sibling module is actually exported there.
//      A browser fails that at load time, with the same blank page.
//
// Run: node dev/check-gui-modules.mjs

import { readdirSync, readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const GUI_DIR = resolve(dirname(fileURLToPath(import.meta.url)), "..", "web", "gui");

// Third-party bundles are shipped as they come; they are not ours to police.
const SKIPPED_DIRS = ["vendor", "proto"];

function guiModules(dir, found = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (!SKIPPED_DIRS.includes(entry.name)) guiModules(path, found);
    } else if (entry.name.endsWith(".js")) {
      found.push(path);
    }
  }
  return found;
}

function parseError(path) {
  try {
    execFileSync(process.execPath, ["--input-type=module", "--check"], {
      input: readFileSync(path),
      stdio: ["pipe", "pipe", "pipe"],
    });
    return null;
  } catch (err) {
    return String(err.stderr).trim().split("\n").slice(0, 3).join("\n");
  }
}

// `import { a, b as c } from "./x.js"` -> the names ./x.js has to export.
const IMPORT = /import\s*\{([^}]*)\}\s*from\s*["'](\.[^"']+)["']/g;
// `import van from "./vendor/van.js"` -> ./vendor/van.js needs a default.
const DEFAULT_IMPORT = /import\s+([A-Za-z0-9_$]+)\s*(?:,\s*\{[^}]*\})?\s*from\s*["'](\.[^"']+)["']/g;
const EXPORT = /export\s+(?:async\s+)?(?:function|class|const|let|var)\s+([A-Za-z0-9_$]+)/g;
const EXPORT_LIST = /export\s*\{([^}]*)\}/g;
const DEFAULT_EXPORT = /export\s+default\s/;

function exportedNames(source) {
  const names = new Set();
  for (const [, name] of source.matchAll(EXPORT)) names.add(name);
  for (const [, list] of source.matchAll(EXPORT_LIST)) {
    for (const part of list.split(",")) {
      const name = part.trim().split(/\s+as\s+/).pop();
      if (name) names.add(name);
    }
  }
  return names;
}

function missingImports(path) {
  const source = readFileSync(path, "utf8");
  const missing = [];

  const read = (target) => {
    try {
      return readFileSync(resolve(dirname(path), target), "utf8");
    } catch {
      missing.push(`imports from '${target}', which does not exist`);
      return null;
    }
  };

  for (const [, list, target] of source.matchAll(IMPORT)) {
    const targetSource = read(target);
    if (targetSource === null) continue;

    const exported = exportedNames(targetSource);
    for (const part of list.split(",")) {
      const name = part.trim().split(/\s+as\s+/)[0];
      if (name && !exported.has(name)) {
        missing.push(`imports '${name}' from '${target}', which does not export it`);
      }
    }
  }

  for (const [, name, target] of source.matchAll(DEFAULT_IMPORT)) {
    const targetSource = read(target);
    if (targetSource === null) continue;

    // `export { x as default }` counts too, and lands in the named set.
    if (!DEFAULT_EXPORT.test(targetSource) && !exportedNames(targetSource).has("default")) {
      missing.push(`imports '${name}' from '${target}', which has no default export`);
    }
  }

  return missing;
}

const failures = [];
const modules = guiModules(GUI_DIR);

for (const path of modules) {
  const relative = path.slice(GUI_DIR.length - "web/gui".length);
  const error = parseError(path);
  if (error) {
    failures.push(`${relative}: does not parse as an ES module\n${error}`);
    // An unparsable file's imports say nothing useful.
    continue;
  }
  for (const problem of missingImports(path)) {
    failures.push(`${relative}: ${problem}`);
  }
}

if (failures.length > 0) {
  console.error(`GUI modules a browser would refuse:\n\n${failures.join("\n\n")}`);
  process.exit(1);
}

console.log(`${modules.length} GUI modules parse and their imports resolve.`);
