// SPDX-License-Identifier: MIT

//! Symbol verification for the hand-written bindings.
//!
//! Decision Q-1 fixes the target at PAPPL 1.3.1 and requires a table asserting
//! that no bound symbol is newer than 1.3. The 1.3.1 headers carry only one
//! `@since` annotation in total, so a per-symbol "introduced in" column cannot
//! be read out of them. What CAN be established mechanically is stronger for
//! our purpose anyway:
//!
//! * every symbol this crate declares is **exported by the installed
//!   libpappl**, which is 1.3.1 — so nothing bound here needs a newer library;
//! * `build.rs` refuses to build against anything outside `>= 1.3, < 2.0`.
//!
//! Together those two mean the binary cannot reference a symbol that 1.3
//! lacks. See `docs/PAPPL-SYMBOLS.md` for the table and the argument.
//!
//! The list below is maintained by hand alongside the declarations in
//! `src/lib.rs`, and `test_symbol_list_matches_the_declarations` checks that
//! the two have not drifted apart.

use std::process::Command;

/// Every function `src/lib.rs` declares in its `extern "C"` block.
const DECLARED: &[&str] = &[
    "papplMainloop",
    "papplMainloopShutdown",
    "papplSystemCreate",
    "papplSystemDelete",
    "papplSystemRun",
    "papplSystemShutdown",
    "papplSystemIsRunning",
    "papplSystemAddListeners",
    "papplSystemSetPrinterDrivers",
    "papplSystemSetLogLevel",
    "papplSystemGetLogLevel",
    "papplSystemLoadState",
    "papplSystemSaveState",
    "papplSystemSetSaveCallback",
    "papplPrinterCreate",
    "papplPrinterDelete",
    "papplPrinterSetDriverData",
    "papplPrinterGetDriverData",
    "papplPrinterSetReadyMedia",
    "papplPrinterGetName",
    "papplPrinterGetID",
    "papplPrinterOpenDevice",
    "papplPrinterCloseDevice",
    "papplPrinterGetReasons",
    "papplPrinterSetReasons",
    "papplJobGetName",
    "papplJobGetUsername",
    "papplJobGetID",
    "papplJobGetFilename",
    "papplJobGetFormat",
    "papplJobGetImpressions",
    "papplJobSetImpressionsCompleted",
    "papplJobIsCanceled",
    "papplJobGetData",
    "papplJobSetData",
    "papplJobSetReasons",
    "papplJobGetPrinter",
    "papplDeviceWrite",
    "papplDevicePuts",
    "papplDeviceFlush",
    "papplDeviceRead",
    "papplDeviceGetStatus",
    "papplLog",
    "papplLogJob",
    "papplLogPrinter",
    "papplCopyString",
];

fn pkg_config(args: &[&str]) -> String {
    let out = Command::new("pkg-config")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("could not run pkg-config: {e}"));
    assert!(out.status.success(), "pkg-config {} failed", args.join(" "));
    String::from_utf8(out.stdout)
        .expect("pkg-config output is UTF-8")
        .trim()
        .to_string()
}

/// Symbols the installed libpappl actually exports.
fn exported() -> Vec<String> {
    let libdir = pkg_config(&["--variable=libdir", "pappl"]);
    let lib = format!("{libdir}/libpappl.so");

    let out = Command::new("nm")
        .args(["-D", "--defined-only", &lib])
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "could not run nm on {lib}: {e}\n\
                 This test needs binutils to check the bound symbols against \
                 the installed library; it does not skip, because unverified \
                 bindings are the thing it exists to prevent."
            )
        });
    assert!(out.status.success(), "nm failed on {lib}");

    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().nth(2).map(str::to_string))
        .collect()
}

#[test]
fn test_every_declared_symbol_is_exported_by_the_installed_libpappl() {
    let exported = exported();
    let missing: Vec<&&str> = DECLARED
        .iter()
        .filter(|s| !exported.iter().any(|e| e == *s))
        .collect();

    assert!(
        missing.is_empty(),
        "{} symbol(s) declared by pappl-sys are not exported by the installed \
         libpappl ({}): {:?}\n\
         Either the declaration is wrong or the library is older than the \
         headers it was built against.",
        missing.len(),
        pkg_config(&["--modversion", "pappl"]),
        missing
    );
}

#[test]
fn test_symbol_list_matches_the_declarations() {
    // The list above is the input to the check, so it must not drift away from
    // the declarations it claims to describe.
    let source = include_str!("../src/lib.rs");
    let declared_in_source: Vec<&str> = source
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pub fn "))
        .filter_map(|l| l.split(['(', '<']).next())
        .collect();

    let mut missing: Vec<&str> = declared_in_source
        .iter()
        .copied()
        .filter(|f| !DECLARED.contains(f))
        .collect();
    missing.sort_unstable();
    assert!(
        missing.is_empty(),
        "declared in src/lib.rs but absent from the symbol list: {missing:?}"
    );

    let mut extra: Vec<&&str> = DECLARED
        .iter()
        .filter(|s| !declared_in_source.contains(s))
        .collect();
    extra.sort();
    assert!(
        extra.is_empty(),
        "in the symbol list but no longer declared in src/lib.rs: {extra:?}"
    );

    assert_eq!(
        declared_in_source.len(),
        DECLARED.len(),
        "the symbol list and the declarations disagree in length"
    );
}
