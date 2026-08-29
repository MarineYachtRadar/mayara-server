//! No unresolved merge conflict stays in the tree.
//!
//! A conflict marker in a `.rs` file cannot compile, so the build catches it.
//! A marker in a GUI file compiles fine, ships inside the binary via
//! `rust-embed`, and only fails in the user's browser -- where it takes the
//! whole ES module with it, so the page renders nothing at all. `node --check`
//! does not catch it either: it parses `web/gui/*.js` as a script rather than
//! a module, and accepts the markers silently.

use std::fs;
use std::path::Path;

/// Directories that hold no source of ours, or hold vast amounts of it.
const SKIPPED: &[&str] = &["target", "node_modules", "testdata", "samples"];

/// Git writes exactly seven characters. Matching that exactly keeps a row of
/// `=` underlining a heading, or a line of `<` in a fixture, from tripping
/// this. Built from fragments so this file cannot match its own test.
fn markers() -> [String; 3] {
    ['<', '=', '>'].map(|c| std::iter::repeat_n(c, 7).collect())
}

/// Whether a line is a conflict marker: the seven characters, then either the
/// end of the line or the space before a branch name.
fn is_marker(line: &str, markers: &[String; 3]) -> bool {
    markers.iter().any(|marker| {
        line.strip_prefix(marker.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    })
}

fn check_dir(dir: &Path, markers: &[String; 3], found: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if path.is_dir() {
            // Dot-directories are `.git`, virtualenvs and editor state --
            // none of it ours.
            if !name.starts_with('.') && !SKIPPED.contains(&name.as_ref()) {
                check_dir(&path, markers, found);
            }
            continue;
        }

        // Binary files (fixtures, images) are read as bytes and skipped.
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };

        for (number, line) in text.lines().enumerate() {
            if is_marker(line, markers) {
                found.push(format!("{}:{}: {}", path.display(), number + 1, line));
            }
        }
    }
}

#[test]
fn no_unresolved_conflict_markers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();

    check_dir(root, &markers(), &mut found);

    assert!(
        found.is_empty(),
        "unresolved merge conflict left in the tree:\n{}",
        found.join("\n")
    );
}
