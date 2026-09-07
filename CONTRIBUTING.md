# Contributing

This is a printer driver. Its output is a byte stream that a machine executes
against paper and toner, and a plausible-looking wrong stream is worse than a
refused job — it wastes consumables and it is not obvious in review. Most of
the rules below exist because of that one fact.

## Ground rules

1. **Never invent the SPL2/QPDL wire protocol.** Every protocol constant,
   command sequence, band or compression detail and page-header field must be
   traceable to code already in this repository, to OpenPrinting SpliX, or to
   a capture. If something is missing, ask — do not reconstruct it from
   memory of a similar printer.
2. **Never invent PAPPL API signatures.** Read the installed headers
   (`pkg-config --cflags pappl`) and quote the real declaration in the review
   description of any FFI change.
3. **Small, reviewable commits.** Do not refactor unrelated code in a change
   that also alters behaviour.
4. `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` must
   pass.
5. **No `unwrap()`, `expect()` or `panic!()` on any path reachable from a C
   callback.** Unwinding across `extern "C"` is undefined behaviour. Callbacks
   go through the `catch_unwind` shim in the `pappl` wrapper crate.
6. **"Do not change behaviour" means byte-for-byte identical output.** The
   golden corpus is the acceptance criterion, not a smoke test.

## The bless discipline

`goldens/` holds 32 frozen SPL2 streams and their sidecars.
`cargo test -p rastertospl-rust golden` compares against them; `UPDATE_GOLDENS=1 cargo test -p rastertospl-rust golden`
overwrites them. **Refreshing — "blessing" — a golden is a reviewed act, never
a way to make a red build green.**

The rule:

* A golden diff means **the bytes going to the printer changed**. Treat it as
  a regression until proven otherwise.
* Refresh only together with a deliberate behaviour change, in the **same
  commit**, and describe in the commit message which bytes moved and why.
* Include the diff in review. `src/golden.rs` prints the first differing
  offset and the surrounding bytes precisely so a reviewer can see what
  changed without reading 28 KB of hex.
* Never refresh to silence an unexplained failure, and never loosen the
  comparison — the `SERVICEDATE` line is pinned rather than ignored for
  exactly this reason (`GOLDEN_SERVICE_DATE`).
* After refreshing, regenerate the checksums: `cd goldens && sha256sum *.spl
  *.json > SHA256SUMS`.

Why the discipline is this strict, in one measured fact:

> **Reordering two job-level PJL lines — `@PJL SET ALTITUDE=LOW` and `@PJL SET
> DENSITY=3` — is caught by the golden corpus and by nothing else in this
> repository. All 106 other tests stay green.**

That result comes from a mutation run in which each of the five migration
risks in `docs/MIGRATION-PLAN.md` §9 was injected as a one-line defect; the
full results are in `docs/GOLDEN-VALIDATION.md`. The unit tests cover the
rules the code is supposed to follow. The corpus is the only thing that covers
the bytes it actually emits.

## What the corpus cannot tell you

The corpus can be self-consistently wrong: every case derives its margin from
the same PPD value, so byte-for-byte agreement proves internal consistency and
nothing about where toner lands on paper. Release gate **G-1**
(`docs/GOLDEN-VALIDATION.md` §4) is the counterweight: 2.0 does not ship until
a registration-mark page has been printed on real hardware and measured with a
ruler. Do not treat a green suite as a verified margin. The procedure is
`docs/G1-MEASUREMENT.md`; the software half of it, which proves the page
survives the driver unchanged, is `scripts/g1-probe.py --all` and it is not
the measurement.

## Language

English for all code, comments, identifiers, commit messages, documentation
and packaging metadata (decision Q-11). This is a GPL project and contributors
will not read Turkish; user-facing localisation, if it ever happens, belongs in
a translation layer, never inlined into source.

## Licensing

`spl2-core` and `ml216x-printer-app` are `GPL-2.0-only`, because the SPL2/QPDL
implementation is derived from GPLv2-only SpliX. `pappl-sys` and `pappl` are
`Apache-2.0 OR MIT`. New files carry the SPDX identifier of the crate they
belong to. See `docs/DECISIONS.md` Q-8a.
