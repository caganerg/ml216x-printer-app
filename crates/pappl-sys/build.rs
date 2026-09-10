// SPDX-License-Identifier: MIT

//! Build script for `pappl-sys`.
//!
//! Two jobs:
//!
//! 1. Locate libpappl through `pkg-config` and emit the link flags. Decision
//!    Q-1 fixes the target at Debian trixie's PAPPL 1.3.1 and guards the range
//!    `>= 1.3, < 2.0`, so an out-of-range library fails the build here rather
//!    than at run time.
//! 2. Compile and run `probe/layout_probe.c` against the installed headers and
//!    save its output for `tests/layout.rs`. The bindings in this crate are
//!    hand written; that test is the only thing that checks the transcription.
//!
//! `pkg-config` is invoked as a program rather than through the `pkg-config`
//! crate on purpose: this project has no third-party Rust dependencies, which
//! keeps `debian/copyright` to a single upstream and avoids adding a
//! build-dependency to the Debian package for a job that is four lines of
//! `Command`.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn pkg_config(args: &[&str]) -> String {
    let out = Command::new("pkg-config")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("could not run pkg-config {}: {e}", args.join(" ")));
    if !out.status.success() {
        panic!(
            "pkg-config {} failed: {}\nIs libpappl-dev installed?",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8(out.stdout)
        .unwrap_or_else(|e| panic!("pkg-config {} returned non-UTF-8: {e}", args.join(" ")))
        .trim()
        .to_string()
}

/// Enforce the `>= 1.3, < 2.0` guard from decision Q-1.
fn check_version(version: &str) {
    let mut parts = version.split('.');
    let major: u32 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("could not parse PAPPL version {version:?}"));
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);

    if major != 1 || minor < 3 {
        panic!(
            "PAPPL {version} is outside the supported range (>= 1.3, < 2.0).\n\
             This crate binds only symbols present in the 1.3 headers; see \
             docs/DECISIONS.md Q-1."
        );
    }
    println!("cargo:rustc-env=PAPPL_SYS_BUILT_AGAINST={version}");
}

fn main() {
    println!("cargo:rerun-if-changed=probe/layout_probe.c");
    println!("cargo:rerun-if-changed=build.rs");

    let version = pkg_config(&["--modversion", "pappl"]);
    check_version(&version);

    for flag in pkg_config(&["--libs", "pappl"]).split_whitespace() {
        if let Some(lib) = flag.strip_prefix("-l") {
            println!("cargo:rustc-link-lib=dylib={lib}");
        } else if let Some(dir) = flag.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={dir}");
        }
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let layout = out_dir.join("pappl_layout.txt");

    // Running the probe requires executing a host binary, so a cross build
    // cannot produce it. Write the reason into the file instead of silently
    // shipping an unchecked layout: tests/layout.rs refuses to run on it.
    if env::var("HOST") != env::var("TARGET") {
        std::fs::write(
            &layout,
            "# unavailable: cross build, the layout probe cannot be executed\n",
        )
        .expect("could not write the layout file");
        return;
    }

    let cflags = pkg_config(&["--cflags", "pappl"]);
    let probe_bin = out_dir.join("layout_probe");
    let cc = env::var("CC").unwrap_or_else(|_| "cc".to_string());

    let status = Command::new(&cc)
        .args(cflags.split_whitespace())
        .arg("-o")
        .arg(&probe_bin)
        .arg("probe/layout_probe.c")
        .status()
        .unwrap_or_else(|e| panic!("could not run the C compiler {cc:?}: {e}"));
    assert!(status.success(), "compiling probe/layout_probe.c failed");

    let out = Command::new(&probe_bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run the layout probe: {e}"));
    assert!(
        out.status.success(),
        "the layout probe exited with an error"
    );

    std::fs::write(&layout, out.stdout).expect("could not write the layout file");
}
