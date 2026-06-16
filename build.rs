use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    install_git_hooks_path();

    let mut src_path = PathBuf::from("src");
    src_path.push("lib");
    src_path.push("protos");
    src_path.push("RadarMessage.proto");

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let mut dest_path = PathBuf::from(&out_dir);
    dest_path.push("lib");
    dest_path.push("protos");
    fs::create_dir_all(&dest_path).unwrap();

    protobuf_codegen::Codegen::new()
        .pure()
        // All inputs and imports from the inputs must reside in `includes` directories.
        .includes(["src/lib/protos"])
        // Inputs must reside in some of include paths.
        .input("src/lib/protos/RadarMessage.proto")
        // Specify output directory relative to Cargo output directory.
        .cargo_out_dir("lib/protos")
        .run_from_script();

    let mut v3_api = PathBuf::from("web");
    v3_api.push("api");
    fs::create_dir_all(&v3_api).unwrap();
    v3_api.push("RadarMessage.proto");
    fs::copy(&src_path, &v3_api).unwrap();

    // Skip GUI download in dev mode - we serve from filesystem instead
    let is_dev = env::var("CARGO_FEATURE_DEV").is_ok();

    // Download GUI from npm if not present (skip in dev mode)
    let gui_dir = PathBuf::from("web").join("gui");
    let mayara_gui_project_dir = PathBuf::from("..").join("mayara-gui");

    if !is_dev
        && !gui_dir.join("viewer.js").exists()
        && mayara_gui_project_dir.join("viewer.js").exists()
    {
        println!("cargo:warning=Copying GUI from ../mayara-gui/...");
        copy_gui_files(&mayara_gui_project_dir, &gui_dir);
    }

    if !is_dev && !gui_dir.join("index.html").exists() {
        println!("cargo:warning=Downloading GUI from npm...");
        // Create temp dir for npm install
        let npm_dir = PathBuf::from(&out_dir).join("npm_temp");
        fs::create_dir_all(&npm_dir).unwrap();
        // Run npm install - use npm.cmd on Windows
        let npm_cmd = if cfg!(windows) { "npm.cmd" } else { "npm" };
        let status = Command::new(npm_cmd)
            .args(["install", "@marineyachtradar/mayara-gui@latest"])
            .current_dir(&npm_dir)
            .status()
            .expect("npm not found - please install Node.js");
        if !status.success() {
            panic!("Failed to download GUI from npm");
        }
        // Copy GUI files from node_modules to OUT_DIR/gui
        let src = npm_dir.join("node_modules/@marineyachtradar/mayara-gui");
        copy_gui_files(&src, &gui_dir);
        // Cleanup npm temp
        let _ = fs::remove_dir_all(&npm_dir);
    }

    println!("cargo:rustc-env=MAYARA_GUI_DIR={}", gui_dir.display());

    // Read api-version from [package.metadata] in Cargo.toml
    let api_version = env::var("CARGO_PKG_METADATA_API_VERSION").unwrap_or_else(|_| {
        // Fallback: parse Cargo.toml directly
        let toml = fs::read_to_string("Cargo.toml").expect("Cannot read Cargo.toml");
        for line in toml.lines() {
            if let Some(v) = line.strip_prefix("api-version") {
                let v = v.trim().trim_start_matches('=').trim().trim_matches('"');
                return v.to_string();
            }
        }
        panic!("api-version not found in [package.metadata]");
    });
    println!("cargo:rustc-env=SIGNALK_RADAR_API_VERSION={}", api_version);

    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=src/protos/RadarMessage.proto");
}

/// Copy GUI files from npm package to destination
/// Only copies relevant files (html, js, css, etc.), excludes package.json etc.
fn copy_gui_files(src: &PathBuf, dest: &PathBuf) {
    fs::create_dir_all(dest).unwrap();

    let extensions = [
        ".html", ".js", ".css", ".ico", ".svg", ".png", ".jpg", ".woff", ".woff2",
    ];
    let directories = ["assets", "proto", "protobuf"];

    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            // Copy known directories
            if directories.contains(&name_str.as_ref()) {
                copy_dir_recursive(&path, &dest.join(&name));
            }
        } else {
            // Copy files with known extensions
            if extensions.iter().any(|ext| name_str.ends_with(ext)) {
                fs::copy(&path, dest.join(&name)).unwrap();
            }
        }
    }
}

/// Recursively copy a directory
fn copy_dir_recursive(src: &PathBuf, dest: &PathBuf) {
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path);
        } else {
            fs::copy(&path, &dest_path).unwrap();
        }
    }
}

/// Point this clone's `core.hooksPath` at `.githooks/` on first `cargo build`,
/// so contributors get the pre-commit `cargo fmt` check without having to
/// run `git config` themselves. Cheap and idempotent:
///
/// - Skips when we're not in a git working tree (e.g. crates.io packaged
///   builds or `cargo vendor` output).
/// - Skips when `.githooks/pre-commit` is missing.
/// - Skips when `core.hooksPath` is already set to anything other than
///   `.githooks` — contributors who configured their own hooks path are
///   left alone (a one-line `cargo:warning=` tells them what they're
///   missing).
fn install_git_hooks_path() {
    println!("cargo:rerun-if-changed=.githooks/pre-commit");

    if !PathBuf::from(".git").is_dir() {
        return;
    }
    if !PathBuf::from(".githooks/pre-commit").is_file() {
        return;
    }

    let current = Command::new("git")
        .args(["config", "--local", "--get", "core.hooksPath"])
        .output();
    let current = match current {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(_) => return, // git not on PATH — nothing useful we can do.
    };

    if current == ".githooks" {
        return; // Already wired up.
    }
    if !current.is_empty() {
        println!(
            "cargo:warning=core.hooksPath is set to '{}'; leaving it alone. \
             Set it to '.githooks' to enable the repo's pre-commit cargo fmt check.",
            current
        );
        return;
    }

    let _ = Command::new("git")
        .args(["config", "--local", "core.hooksPath", ".githooks"])
        .status();
}
