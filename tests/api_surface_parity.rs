//! Cross-project Radar API surface parity.
//!
//! mayara-server, signalk-server, and the signalk-plugin must present an
//! identical Radar API so a client cannot tell whether it is talking to mayara
//! directly or through Signal K. This test enforces that the shared surface —
//! the Radar API `version` and the `RadarInfo` field set — stays in lockstep.
//!
//! It is gated on a committed snapshot of mayara's own surface
//! (`tests/api_surface.snapshot`): while the surface is unchanged the test
//! returns immediately, doing no cross-project or git work — that is the fast
//! path on every `cargo test`. Only when mayara's Radar API surface *changes*
//! does it verify the sister projects, which must be:
//!   - checked out next to this repo (`../signalk-server`, `../mayara-server-signalk-plugin`),
//!   - on a clean default branch (`main`/`master`), and
//!   - not behind their already-fetched upstream.
//!
//! The test is read-only and offline: it never fetches, pulls, or otherwise
//! mutates a sister checkout. CI must fetch and prepare them beforehand.
//!
//! If a sister is missing, not on its default branch, dirty, or its surface
//! differs, the test fails.
//!
//! After an intentional API change, regenerate the snapshot:
//!     MAYARA_UPDATE_API_SNAPSHOT=1 cargo test --test api_surface_parity

use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// mayara's documented `RadarInfo` extensions: present in mayara, but not part
/// of the shared surface (and absent from signalk-server). Clients ignore them.
const MAYARA_RADARINFO_EXTENSIONS: &[&str] = &["replay"];

#[test]
fn radar_api_surface_identical_across_projects() {
    let current = mayara_surface();
    let snapshot_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/api_surface.snapshot");

    if std::env::var_os("MAYARA_UPDATE_API_SNAPSHOT").is_some() {
        std::fs::write(&snapshot_path, &current).expect("write snapshot");
        eprintln!("Updated {}:\n{current}", snapshot_path.display());
        return;
    }

    let snapshot = std::fs::read_to_string(&snapshot_path).unwrap_or_default();
    if current.trim() == snapshot.trim() {
        // Surface unchanged since the snapshot — nothing to verify. The
        // cross-project + git work below runs only when the surface changes.
        return;
    }

    eprintln!(
        "Radar API surface changed vs snapshot — verifying sister projects.\n\
         current:\n{current}\nsnapshot:\n{snapshot}"
    );

    let root = workspace_root();
    let signalk = root.join("signalk-server");
    let plugin = root.join("mayara-server-signalk-plugin");

    for (name, path) in [("signalk-server", &signalk), ("signalk-plugin", &plugin)] {
        assert!(
            path.is_dir(),
            "sister project '{name}' not found at {} — check it out next to \
             mayara-server to verify Radar API parity",
            path.display()
        );
        ensure_clean_default_branch_uptodate(name, path);
    }

    let sk = signalk_server_surface(&signalk);
    assert_eq!(
        current.trim(),
        sk.trim(),
        "\nRadar API surface diverged between mayara-server and signalk-server.\n\
         Update both to match (and bump the version in lockstep).\n"
    );

    // Surface changed and the sisters agree. Force the snapshot to be
    // regenerated so the fast path is restored and the change is acknowledged.
    panic!(
        "\nRadar API surface changed and matches the sister projects.\n\
         Record the new surface:\n\
         \x20   MAYARA_UPDATE_API_SNAPSHOT=1 cargo test --test api_surface_parity\n"
    );
}

/// The shared Radar API surface mayara presents: `version` + sorted `RadarInfo`
/// field names, excluding mayara's documented extensions.
fn mayara_surface() -> String {
    let version = mayara::SIGNALK_RADAR_API_VERSION;
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/mayara-server/web/signalk/v2.rs"),
    )
    .expect("read v2.rs");
    let mut fields: Vec<String> = rust_struct_fields(&src, "RadarApiV3")
        .into_iter()
        .map(|f| snake_to_camel(&f))
        .filter(|f| !MAYARA_RADARINFO_EXTENSIONS.contains(&f.as_str()))
        .collect();
    fields.sort();
    surface_descriptor(version, &fields)
}

/// The shared Radar API surface signalk-server presents.
fn signalk_server_surface(sk: &Path) -> String {
    let index = std::fs::read_to_string(sk.join("src/api/radar/index.ts"))
        .expect("read signalk-server src/api/radar/index.ts");
    let version = extract_between(&index, "RADAR_API_VERSION = '", "'")
        .expect("RADAR_API_VERSION not found in signalk-server index.ts");
    let radarapi = std::fs::read_to_string(sk.join("packages/server-api/src/radarapi.ts"))
        .expect("read signalk-server packages/server-api/src/radarapi.ts");
    let mut fields = ts_interface_fields(&radarapi, "RadarInfo");
    fields.sort();
    surface_descriptor(&version, &fields)
}

fn surface_descriptor(version: &str, fields: &[String]) -> String {
    format!("version={}\nfields={}\n", version, fields.join(","))
}

// ---------------------------------------------------------------------------
// Source extraction (deliberately dependency-free string parsing)
// ---------------------------------------------------------------------------

/// Field names declared in a Rust `struct <name> { ... }`.
fn rust_struct_fields(src: &str, name: &str) -> Vec<String> {
    let open = format!("struct {name} {{");
    let start = src
        .find(&open)
        .unwrap_or_else(|| panic!("struct {name} not found"));
    let mut fields = Vec::new();
    for line in src[start + open.len()..].lines() {
        let t = line.trim();
        if t == "}" {
            break;
        }
        if t.is_empty() || t.starts_with('#') || t.starts_with("//") {
            continue;
        }
        if let Some((ident, _)) = t.split_once(':') {
            let ident = ident.trim();
            if !ident.is_empty() && ident.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                fields.push(ident.to_string());
            }
        }
    }
    fields
}

/// Field names declared in a TypeScript `interface <name> { ... }`.
fn ts_interface_fields(src: &str, name: &str) -> Vec<String> {
    let open = format!("interface {name} {{");
    let start = src
        .find(&open)
        .unwrap_or_else(|| panic!("interface {name} not found"));
    let mut fields = Vec::new();
    for line in src[start + open.len()..].lines() {
        let t = line.trim();
        if t == "}" {
            break;
        }
        if t.is_empty() || t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') {
            continue;
        }
        if let Some((ident, _)) = t.split_once(':') {
            let ident = ident.trim().trim_end_matches('?');
            if !ident.is_empty() && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                fields.push(ident.to_string());
            }
        }
    }
    fields
}

fn extract_between(src: &str, start: &str, end: &str) -> Option<String> {
    let s = src.find(start)? + start.len();
    let rest = &src[s..];
    let e = rest.find(end)?;
    Some(rest[..e].to_string())
}

fn snake_to_camel(s: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in s.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Sister project git state
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("mayara-server has a parent directory")
        .to_path_buf()
}

fn run_git(path: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run `git {}`: {e}", args.join(" ")));
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

/// Require the sister to be on a clean default branch that is not behind its
/// already-fetched upstream, so the parity comparison is against the canonical
/// version. Read-only and offline: the test never fetches, pulls, or otherwise
/// mutates a sister checkout — CI is responsible for fetching them first.
fn ensure_clean_default_branch_uptodate(name: &str, path: &Path) {
    // The repo's default branch (origin/HEAD → e.g. "main" or "master").
    let (ok, head, _) = run_git(
        path,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    );
    let default_branch = if ok {
        head.strip_prefix("origin/").unwrap_or(&head).to_string()
    } else if run_git(path, &["rev-parse", "--verify", "main"]).0 {
        "main".to_string()
    } else {
        "master".to_string()
    };

    let (_, current, _) = run_git(path, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(
        current, default_branch,
        "sister '{name}' is on branch '{current}', not its default branch \
         '{default_branch}'. Check it out on '{default_branch}' to verify Radar API parity."
    );

    let (_, status, _) = run_git(path, &["status", "--porcelain"]);
    assert!(
        status.is_empty(),
        "sister '{name}' has uncommitted changes; it must be clean to verify \
         Radar API parity:\n{status}"
    );

    // Compare against the upstream ref already in the local object store. If
    // there is no upstream configured there is nothing to check — the local
    // branch is canonical as far as this checkout is concerned.
    let (ok, upstream, _) = run_git(
        path,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    );
    if !ok {
        return;
    }
    let (_, behind, _) = run_git(path, &["rev-list", "--count", &format!("HEAD..{upstream}")]);
    assert_eq!(
        behind, "0",
        "sister '{name}' is {behind} commit(s) behind '{upstream}'; fetch and \
         fast-forward it before verifying Radar API parity"
    );
}
